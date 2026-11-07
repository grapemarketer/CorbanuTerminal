"""Portable synthetic child tests of execute(); no native auth, models or OS admission.

Run with Python >=3.11, -B. Retain each test's private attempts in printed temp root.
Only test code replaces launch/platform/CA seams; production has no unsafe switch.
"""
from dataclasses import FrozenInstanceError
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import struct
import sys
import tempfile
import time
import unittest
import zlib
from unittest.mock import patch

import isolated_model_transport as m

EVIDENCE = None
REAL_LAUNCH = m._launch
AUTH = json.dumps({'auth_mode': 'chatgpt', 'tokens': {
    'id_token': 'SYNTHETIC_ID', 'access_token': 'SYNTHETIC_ACCESS',
    'refresh_token': 'SYNTHETIC_REFRESH'}}).encode()


def chunk(kind, data=b''):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))


def png(width=1, height=1, depth=8, color=6, compression=0, filtering=0, interlace=0, payload=None):
    header = struct.pack('>IIBBBBB', width, height, depth, color, compression, filtering, interlace)
    pixels = b'\0' * (height * (1 + width * (3 if color == 2 else 4)))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header)
            + chunk(b'IDAT', zlib.compress(pixels) if payload is None else payload) + chunk(b'IEND'))


CHILD = r'''
import json, os, sys, time, uuid
from pathlib import Path
mode = sys.argv[1]
run = Path.cwd()
session = str(uuid.uuid4()) if mode != 'reuse' else '00000000-0000-4000-8000-000000000001'
events = [dict(type='thread.started', thread_id=session), dict(type='turn.started'),
          dict(type='item.completed', item=dict(id='r', type='reasoning', text='PRIVATE_REASONING')),
          dict(type='item.completed', item=dict(id='a', type='agent_message', text='SAFE_TEXT')),
          dict(type='turn.completed', usage=dict(input_tokens=1, cached_input_tokens=0, output_tokens=1))]
if mode in ('blocked_input', 'closed_outputs'):
    if mode == 'closed_outputs':
        os.close(1); os.close(2)
    time.sleep(10)
if mode == 'duplex':
    os.write(2, b'd' * 131072)
packet = sys.stdin.buffer.read()
(run / 'packet.raw').write_bytes(packet)
native = json.loads(sys.argv[3])
if '--image' in native:
    image = Path(native[native.index('--image') + 1])
    (run / 'observed-image.raw').write_bytes(image.read_bytes())
    if mode == 'image_mutation': image.write_bytes(b'CHANGED_IMAGE')
    if mode == 'image_replacement':
        replacement = run / 'replacement.png'; replacement.write_bytes(image.read_bytes())
        replacement.chmod(0o600); replacement.replace(image)
    if mode == 'image_symlink':
        target = run / 'target.png'; target.write_bytes(image.read_bytes()); target.chmod(0o600)
        image.unlink(); image.symlink_to(target)
    if mode == 'image_hardlink': os.link(image, run / 'linked.png')
    if mode == 'image_permissions': image.chmod(0o644)
    if mode == 'image_deleted': image.unlink()
if mode == 'auth_mutation':
    (run / 'home/auth.json').write_bytes(b'SYNTHETIC_MUTATED')
if mode == 'auth_replacement':
    target = (run / 'home/auth.json').resolve()
    value = target.read_bytes(); target.unlink(); target.write_bytes(value); target.chmod(0o600)
if mode == 'link_mutation':
    link = run / 'home/auth.json'; link.unlink(); link.symlink_to(run / 'model-only.json')
if mode == 'catalog_mutation':
    (run / 'model-only.json').write_bytes(b'{}')
if mode == 'ca_mutation':
    (run.parent / 'ca').write_bytes(b'CHANGED_SYNTHETIC_CA')
if mode.startswith('warning'):
    warning = json.loads(sys.argv[2])
    if mode == 'warning_altered': warning += ' '
    events.insert(0, dict(type='item.completed', item=dict(id='w', type='error', message=warning)))
if mode in ('error', 'turn.failed'):
    events.insert(-1, dict(type=mode, message='PRIVATE_RUNTIME_ERROR'))
if mode in ('unknown', 'tool'):
    events.insert(-1, dict(type='surprise') if mode == 'unknown' else
                  dict(type='item.completed', item=dict(id='t', type='command_execution', command='NO')))
if mode == 'missing': events.pop()
if mode == 'duplicate_thread': events.insert(1, events[0])
if mode == 'duplicate_turn': events.append(events[-1])
if mode == 'duplicate_item': events.insert(-1, events[-2])
if mode == 'malformed_usage': events[-1]['usage']['output_tokens'] = True
if mode == 'null_usage': events[-1]['usage'] = None
if mode == 'unknown_field': events[-2]['item']['error'] = 'PRIVATE_RUNTIME_ERROR'
if mode == 'bad_session': events[0]['thread_id'] = '../../PRIVATE_PATH'
if mode == 'late_error': events.append(dict(type='error', message='PRIVATE_RUNTIME_ERROR'))
if mode.startswith('unicode'):
    events[2]['item']['text'] = 'PRIVATE\u0085\u2028\u2029REASONING'
    events[3]['item']['text'] = 'SAFE\u0085\u2028\u2029TEXT'
raw = b''.join(json.dumps(e, ensure_ascii=not mode.startswith('unicode')).encode() + b'\n' for e in events)
if mode == 'unicode_crlf': raw = raw.replace(b'\n', b'\r\n')
if mode == 'unicode_no_final_lf': raw = raw[:-1]
if mode == 'blank_record': raw += b'\n'
if mode == 'malformed': raw += b'not json\n'
if mode == 'invalid_utf8': raw += b'\xff\n'
if mode == 'scalar': raw += b'[]\n'
if mode == 'duplicate_key': raw += b'{"type":"error","type":"turn.started"}\n'
if mode == 'surrogate': raw = raw.replace(b'SAFE_TEXT', b'\\ud800')
if mode in ('exact_output', 'oversize'):
    os.write(2, b'd' * (262144 - len(raw) + (mode == 'oversize')))
os.write(1, raw)
sys.exit(7 if mode == 'nonzero' else 0)
'''


class TransportTests(unittest.TestCase):
    def setUp(self):
        global EVIDENCE
        if EVIDENCE is None:
            EVIDENCE = Path(tempfile.mkdtemp(prefix='isolated-transport-tests-')).resolve()
            print('Synthetic retained evidence:', EVIDENCE, flush=True)
        self.root = Path(tempfile.mkdtemp(prefix=self._testMethodName + '-', dir=EVIDENCE))
        self.binary, self.auth, self.ca = (self.root / n for n in ('binary', 'auth', 'ca'))
        for path, data in ((self.binary, b'SYNTHETIC_BINARY'), (self.auth, AUTH),
                           (self.ca, b'SYNTHETIC_PUBLIC_CA')):
            path.write_bytes(data)
            path.chmod(0o600)
        self.binary.chmod(0o700)
        self.owner = m.OwnerConfig(self.binary, self.auth, self.root, enabled=True)
        self.transport = m.Transport(self.owner)
        self.mode = 'normal'
        self.calls = []
        self.native_calls = []
        original_fingerprint = m._fingerprint

        def fingerprint(path, **checks):
            if path == self.ca:
                checks.pop('root', None)  # Synthetic CA belongs to fixture owner.
            return original_fingerprint(path, **checks)

        def launch(owner, run, *, with_image=False):
            self.calls.append((owner, run))
            native, env = REAL_LAUNCH(owner, run, with_image=with_image)
            self.native_calls.append((native, env))
            m._save(run / 'synthetic-native-launch.json', dict(argv=native, environment=env))
            return (sys.executable, '-I', '-B', '-c', CHILD, self.mode,
                    json.dumps(sorted(m._WARNINGS)[0]), json.dumps(native)), {'PATH': '/usr/bin:/bin'}

        for name, value in (('_CA', self.ca), ('BINARY_SHA', hashlib.sha256(self.binary.read_bytes()).hexdigest()),
                            ('_fingerprint', fingerprint), ('_launch', launch), ('_WALL', 2)):
            self.enterContext(patch.object(m, name, value))
        self.enterContext(patch.object(m.sys, 'platform', 'darwin'))

    def attempt(self, mode='normal', text='SYNTHETIC_INPUT', expected='ok', image_png=None):
        self.mode = mode
        count = len(self.calls)
        result = self.transport.execute(text, image_png=image_png)
        self.assertEqual(result['status'], expected, dict(result))
        self.assertLessEqual(len(self.calls) - count, 1)  # No transport retry or fallback.
        run = self.root / result['attempt_id']
        self.assertTrue((run / 'receipt.json').exists())
        self.assertLessEqual(sum((run / n).stat().st_size for n in ('stdout.raw', 'stderr.raw')), m._OUTPUT)
        if 'owned_group_gone' in result:
            self.assertTrue(result['owned_group_gone'])
        if expected != 'ok':
            self.assertIsNone(result['text'])
            self.assertTrue((run / 'failure.json').exists())
        return result, run

    def test_inline_png_content_identity_exact_argv_and_policy(self):
        for color in (2, 6):
            data = png(color=color)
            result, run = self.attempt(image_png=data)
            self.assertEqual((run / 'observed-image.raw').read_bytes(), data)
            self.assertEqual((run / 'packet.raw').read_bytes(), b'SYNTHETIC_INPUT')
            self.assertEqual(result['image_sha256'], m._digest(data))
            self.assertEqual(result['image_observation'], 'supplied_not_attested')
            self.assertEqual((result['image_bytes'], result['image_width'], result['image_height']), (len(data), 1, 1))
            pin = json.loads((run / 'image.json').read_text())['identity']
            self.assertEqual(pin, list(m._fingerprint(run / 'observation.png', private=True)))
            native, env = self.native_calls[-1]
            plain, plain_env = REAL_LAUNCH(self.owner, run)
            self.assertEqual(native[3:], (*plain[3:-1], '--image', str(run / 'observation.png'), '--', '-'))
            self.assertEqual(native[2], plain[2] + '\n(allow file-read* (literal ' + json.dumps(str(run / 'observation.png')) + '))')
            self.assertEqual(env, plain_env)
            self.assertTrue(result['inputs_unchanged'])
        _, run = self.attempt()
        self.assertNotIn('--image', self.native_calls[-1][0])
        self.assertFalse((run / 'observation.png').exists())

    def test_png_rejections_before_files_auth_or_process(self):
        data = png()
        header, idat, end = data[8:33], data[33:-12], data[-12:]
        invalid = [b'', 'file.png', Path('/image.png'), 'https://example.test/image.png',
                   bytearray(data), memoryview(data), [data], data + b'x', data[:-1],
                   b'x' + data[1:], data[:40] + bytes([data[40] ^ 1]) + data[41:],
                   data[:33] + b'\xff\xff\xff\xff' + data[37:], b'x' * (m._IMAGE_BYTES + 1),
                   png(0), png(height=0), png(4097), png(height=4097), png(2049, 2048),
                   png(depth=16), png(color=3), png(compression=1), png(filtering=1), png(interlace=1)]
        for middle in (idat + header, header + header + idat, header, header + chunk(b'IDAT'),
                       header + chunk(b'tEXt', b'key\0value') + idat, header + chunk(b'acTL') + idat,
                       header + chunk(b'IDAT') * 1023 + idat):
            invalid.append(data[:8] + middle + end)
        invalid += [data[:-12], data[:-12] + chunk(b'IEND', b'x'), data + end]
        with patch.object(m, '_path', side_effect=AssertionError('unexpected path read')), \
             patch.object(m, '_snapshots', side_effect=AssertionError('unexpected auth read')), \
             patch.object(m, '_launch', side_effect=AssertionError('unexpected process')):
            for index, value in enumerate(invalid):
                with self.subTest(index=index), self.assertRaises(ValueError):
                    self.transport.execute('text', image_png=value)
        self.assertEqual(self.calls, [])
        self.assertEqual(list(self.root.glob('attempt-*')), [])

    def test_png_pixel_bounds_and_split_idat(self):
        for data in (png(4096, 1), png(1, 4096), png(2048, 2048)):
            self.attempt(image_png=data)
        data = png()
        self.attempt(image_png=data[:33] + chunk(b'IDAT') + data[33:])
        payload = zlib.compress(b'\0' * 5)
        self.attempt(image_png=data[:33] + chunk(b'IDAT', payload[:3]) + chunk(b'IDAT', payload[3:]) + chunk(b'IEND'))

    def test_bad_compressed_pixels_rejected_before_auth_or_process(self):
        payload = zlib.compress(b'\0' * 5)
        invalid = [b'not deflate', payload[:-1], payload + b'x', payload + payload,
                   zlib.compress(b'\0' * 4), zlib.compress(b'\0' * 6),
                   zlib.compress(b'\0' * 1000000), zlib.compress(b'\5' + b'\0' * 4)]
        with patch.object(m, '_path', side_effect=AssertionError('unexpected path read')), \
             patch.object(m, '_snapshots', side_effect=AssertionError('unexpected auth read')), \
             patch.object(m, '_launch', side_effect=AssertionError('unexpected process')):
            for index, compressed in enumerate(invalid):
                with self.subTest(index=index), self.assertRaises(ValueError):
                    self.transport.execute('text', image_png=png(payload=compressed))
        self.assertEqual(self.calls, [])
        self.assertEqual(list(self.root.glob('attempt-*')), [])
        for filtering in range(5):
            self.attempt(image_png=png(payload=zlib.compress(bytes([filtering]) + b'\0' * 4)))

    def test_image_mutations_fail_with_retained_original_pin_and_diagnostics(self):
        data = png()
        for mode in ('image_mutation', 'image_replacement', 'image_symlink',
                     'image_hardlink', 'image_permissions', 'image_deleted'):
            with self.subTest(mode=mode):
                result, run = self.attempt(mode, expected='failed', image_png=data)
                self.assertEqual(result['reason'], 'inputs_changed')
                self.assertFalse(result['inputs_unchanged'])
                self.assertEqual((run / 'observed-image.raw').read_bytes(), data)
                self.assertEqual(json.loads((run / 'image.json').read_text())['identity'][-1], m._digest(data))
                self.assertTrue((run / 'stdout.raw').stat().st_size)

    def test_image_prelaunch_change_and_exclusive_collision_refused(self):
        original_launch, original_save = m._launch, m._save
        for mutation in ('bytes', 'symlink', 'replacement'):
            def changed(owner, run, **kwargs):
                result = original_launch(owner, run, **kwargs)
                path = run / 'observation.png'
                if mutation == 'bytes': path.write_bytes(b'CHANGED')
                elif mutation == 'symlink':
                    path.unlink(); path.symlink_to(run / 'model-only.json')
                else:
                    other = run / 'replacement.png'; original_save(other, path.read_bytes()); other.replace(path)
                return result
            with patch.object(m, '_launch', changed), patch.object(m, '_bounded', side_effect=AssertionError('must not spawn')):
                _, run = self.attempt(expected='failed', image_png=png())
                self.assertIn('ValueError', (run / 'failure.json').read_text())
        for symlink in (False, True):
            def collide(path, data):
                if path.name == 'observation.png':
                    if symlink: path.symlink_to(self.ca)
                    else: original_save(path, b'EXISTING')
                original_save(path, data)
            count = len(self.calls)
            with patch.object(m, '_save', collide):
                _, run = self.attempt(expected='failed', image_png=png())
            self.assertIn('FileExistsError', (run / 'failure.json').read_text())
            self.assertEqual(len(self.calls), count)
            self.assertEqual((run / 'observation.png').read_bytes(), self.ca.read_bytes() if symlink else b'EXISTING')

    def test_image_off_is_inert_and_comma_root_rejected(self):
        off = m.Transport(m.OwnerConfig(self.binary, self.auth, self.root))
        with patch.object(m, '_png', side_effect=AssertionError('OFF validated image')), \
             patch.object(m, '_path', side_effect=AssertionError('OFF path')), \
             patch.object(m.subprocess, 'Popen', side_effect=AssertionError('OFF process')):
            for value in (png(), b'broken', object(), b'x' * (m._IMAGE_BYTES + 1)):
                self.assertEqual(dict(off.execute(object(), image_png=value)), {'status': 'off', 'text': None})
        owner = m.OwnerConfig(self.binary, self.auth, self.root / 'comma,root', True)
        with patch.object(m, '_path', side_effect=AssertionError('preflight path')):
            with self.assertRaisesRegex(ValueError, 'delimiter'):
                m.Transport(owner).execute('text', image_png=png())

    def test_fresh_success_private_receipts_and_no_raw_leak(self):
        first, a = self.attempt()
        second, b = self.attempt()
        self.assertNotEqual(a, b)
        self.assertNotEqual(first['session_id'], second['session_id'])
        self.assertEqual(first['text'], 'SAFE_TEXT')
        self.assertNotIn('PRIVATE_REASONING', json.dumps(dict(first)))
        self.assertEqual((a / 'packet.raw').read_bytes(), b'SYNTHETIC_INPUT')
        self.assertEqual(os.readlink(a / 'home/auth.json'), str(self.auth))
        self.assertTrue(first['inputs_unchanged'])
        self.assertEqual((a / 'home').stat().st_mode & 0o777, 0o700)
        for name in ('launch.json', 'receipt.json', 'stdout.raw', 'stderr.raw', 'model-only.json'):
            self.assertEqual((a / name).stat().st_mode & 0o777, 0o600)
        with self.assertRaises(TypeError):
            first['status'] = 'changed'

    def test_utf8_byte_bound_and_full_duplex(self):
        for text in ('x' * 65536, '\u00e9' * 32768, ''):
            result, run = self.attempt('duplex', text)
            self.assertEqual(result['input_bytes_sent'], len(text.encode()))
            self.assertEqual((run / 'packet.raw').read_bytes(), text.encode())
        count = len(self.calls)
        for text in ('x' * 65537, '\u00e9' * 32769, '\ud800', b'bytes', {'url': 'no'}):
            with self.assertRaises((ValueError, UnicodeError)):
                self.transport.execute(text)
        self.assertEqual(len(self.calls), count)

    def test_aggregate_exact_and_overflow(self):
        _, run = self.attempt('exact_output')
        self.assertEqual(sum((run / n).stat().st_size for n in ('stdout.raw', 'stderr.raw')), 262144)
        result, _ = self.attempt('oversize', expected='failed')
        self.assertEqual(result['reason'], 'output_limit')

    def test_unicode_text_is_not_a_jsonl_record_boundary(self):
        for mode in ('unicode', 'unicode_crlf', 'unicode_no_final_lf'):
            with self.subTest(mode=mode):
                result, _ = self.attempt(mode)
                self.assertEqual(result['text'], 'SAFE\u0085\u2028\u2029TEXT')
        self.attempt('blank_record', expected='failed')

    def test_input_wait_and_closed_output_timeout_cleanup(self):
        for mode in ('blocked_input', 'closed_outputs'):
            with self.subTest(mode=mode), patch.object(m, '_WALL', .3):
                started = time.monotonic()
                result, _ = self.attempt(mode, 'x' * 65536, expected='failed')
                self.assertEqual(result['reason'], 'timeout')
                self.assertLess(time.monotonic() - started, 2)

    def test_malformed_unknown_errors_never_promote_text(self):
        for mode in ('error', 'turn.failed', 'unknown', 'tool', 'missing', 'duplicate_thread',
                     'duplicate_turn', 'duplicate_item', 'malformed_usage', 'unknown_field',
                     'bad_session', 'late_error', 'malformed', 'invalid_utf8', 'scalar',
                     'duplicate_key', 'surrogate', 'nonzero'):
            with self.subTest(mode=mode):
                result, run = self.attempt(mode, expected='failed')
                self.assertEqual(len(self.calls), len(list(self.root.glob('attempt-*'))))
                self.assertNotIn('PRIVATE_RUNTIME_ERROR', json.dumps(dict(result)))
                self.assertTrue((run / 'stdout.raw').stat().st_size)

    def test_null_usage_is_accepted(self):
        result, _ = self.attempt('null_usage')
        self.assertEqual(result['text'], 'SAFE_TEXT')

    def test_only_exact_known_warnings(self):
        result, _ = self.attempt('warning')
        self.assertEqual(result['known_warning_count'], 1)
        self.attempt('warning_altered', expected='failed')

    def test_source_auth_catalog_and_link_mutations(self):
        for mode in ('auth_mutation', 'auth_replacement', 'catalog_mutation', 'link_mutation', 'ca_mutation'):
            with self.subTest(mode=mode):
                self.transport = m.Transport(self.owner)
                self.auth.write_bytes(AUTH)
                result, _ = self.attempt(mode, expected='failed')
                self.assertFalse(result['inputs_unchanged'])
        self.assertEqual(len(self.calls), 5)

    def test_auth_immutable_across_requests(self):
        self.attempt()
        self.auth.write_bytes(b'NEW_SYNTHETIC_AUTH')
        self.attempt(expected='failed')
        self.assertEqual(len(self.calls), 1)

    def test_fresh_session_refuses_replay(self):
        self.attempt('reuse')
        self.attempt('reuse', expected='failed')
        self.assertEqual(len(self.calls), 2)

    def test_native_chatgpt_mode_required_and_nonexecutable_refused(self):
        for data in (b'{}', b'{"OPENAI_API_KEY":"SYNTHETIC_API_KEY"}',
                     AUTH.replace(b'chatgpt', b'apikey'), b'[]', b'{broken'):
            self.auth.write_bytes(data)
            self.attempt(expected='failed')
        self.auth.write_bytes(AUTH)
        self.binary.chmod(0o600)
        self.attempt(expected='failed')
        self.assertEqual(self.calls, [])

    def test_busy_does_not_wait_or_launch(self):
        with self.transport._lock:
            with self.assertRaisesRegex(ValueError, 'busy'):
                self.transport.execute('text')
        self.assertEqual(self.calls, [])

    def test_pin_refusals_do_not_launch(self):
        for name in ('BINARY_SHA', 'CATALOG_SHA'):
            with self.subTest(pin=name), patch.object(m, name, '0' * 64):
                self.attempt(expected='failed')
        self.assertEqual(self.calls, [])

    def test_source_catalog_refuses_changes_and_aliases(self):
        catalog = self.root / 'catalog.json'
        catalog.write_bytes(b'{}')
        with patch.object(m, '_CATALOG', catalog):
            self.attempt(expected='failed')
        catalog.unlink()
        catalog.symlink_to(m._CATALOG)
        with patch.object(m, '_CATALOG', catalog):
            self.attempt(expected='failed')
        self.assertEqual(self.calls, [])

    def test_paths_permissions_hardlinks_and_symlinks(self):
        for field in ('binary', 'auth'):
            original = getattr(self.owner, field)
            alias = self.root / (field + '-alias')
            alias.symlink_to(original)
            self.transport = m.Transport(m.OwnerConfig(**{**self.owner.__dict__, field: alias}))
            self.attempt(expected='failed')
            alias.unlink()
            os.link(original, alias)
            self.transport = m.Transport(self.owner)
            self.attempt(expected='failed')
            alias.unlink()
        self.auth.chmod(0o644)
        self.attempt(expected='failed')
        self.assertEqual(self.calls, [])
        self.root.chmod(0o755)
        with self.assertRaises(ValueError):
            self.transport.execute('text')
        self.root.chmod(0o700)
        alias = self.root / 'root-alias'
        alias.symlink_to(self.root)
        with self.assertRaises(ValueError):
            m.Transport(m.OwnerConfig(self.binary, self.auth, alias, True)).execute('text')

    def test_immutable_closed_owner_api_and_off(self):
        with self.assertRaises(FrozenInstanceError):
            self.owner.auth = self.ca
        with self.assertRaises(FrozenInstanceError):
            self.transport.owner = self.owner
        with self.assertRaises(TypeError):
            m._FIXED['model_provider'] = 'other'
        for keyword in ('model', 'effort', 'url', 'headers', 'argv', 'tools', 'auth', 'run_root'):
            with self.assertRaises(TypeError):
                self.transport.execute('text', **{keyword: 'UNTRUSTED'})
        with patch.object(m, '_path', side_effect=AssertionError('OFF read')), patch.object(m.subprocess, 'Popen', side_effect=AssertionError('OFF spawn')):
            off = m.Transport(m.OwnerConfig(self.binary, self.auth, self.root))
            self.assertEqual(dict(off.execute('text')), {'status': 'off', 'text': None})

    def test_real_builder_fixed_native_endpoint_and_policy(self):
        argv, env = REAL_LAUNCH(self.owner, self.root / 'attempt-owner')
        self.assertEqual(argv[:2], ('/usr/bin/sandbox-exec', '-p'))
        self.assertIn('model_reasoning_effort="high"', argv)
        self.assertIn('gpt-6-astra', argv)
        self.assertFalse(any('base_url' in arg or 'http_headers' in arg for arg in argv))
        for key in ('request_max_retries', 'stream_max_retries'):
            self.assertIn('model_providers.isolated-openai-transport.' + key + '=0', argv)
        self.assertEqual(env['SSL_CERT_FILE'], str(self.ca))
        self.assertEqual(len(env), 7)
        guard = argv[2]
        for fragment in ('(deny default)', '"*:443"', '"/private/var/run/mDNSResponder"',
                         'com.apple.securityd', '(deny file-write*', '/Keychains(/|$)'):
            self.assertIn(fragment, guard)
        self.assertNotIn('allow process-fork', guard)
        self.assertEqual(guard.count('(allow process-exec'), 1)
        self.assertNotIn('(subpath ' + json.dumps(str(self.root)) + ')', guard)

    def test_import_help_optimized_refusal_and_no_dynamic_source_import(self):
        code = '''import sys
def audit(event, args):
    if event in ('subprocess.Popen', 'os.system', 'socket.connect'): raise RuntimeError(event)
    if event == 'open' and isinstance(args[0], str) and ('auth.json' in args[0] or 'Keychains' in args[0]): raise RuntimeError('auth read')
sys.addaudithook(audit)
import isolated_model_transport as m
from pathlib import Path
owner = m.OwnerConfig(Path('/absent/binary'), Path('/absent/auth.json'), Path('/absent/runs'))
if m.Transport(owner).execute('text')['status'] != 'off': raise RuntimeError('OFF')
try: m.OwnerConfig(Path('relative'), owner.auth, owner.run_root)
except ValueError: pass
else: raise RuntimeError('optimized gate lost')
sys.argv = [m.__file__, '--help']
import runpy
runpy.run_path(m.__file__, run_name='__main__')
'''
        for optimization in ([], ['-O']):
            result = subprocess.run([sys.executable, '-B', *optimization, '-c', code],
                                    cwd=Path(m.__file__).parent, env={'PATH': '/usr/bin:/bin'},
                                    capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(b'Owner-only library', result.stdout)
        import ast
        tree = ast.parse(Path(m.__file__).read_text())
        self.assertFalse(any(isinstance(n, ast.Assert) for n in ast.walk(tree)))

    def test_launch_failure_retains_attempt(self):
        with patch.object(m.subprocess, 'Popen', side_effect=OSError('SYNTHETIC_LAUNCH_FAILURE')):
            result, run = self.attempt(expected='failed')
        self.assertEqual(result['reason'], 'launch')
        self.assertIn('SYNTHETIC_LAUNCH_FAILURE', (run / 'failure.json').read_text())


if __name__ == '__main__':
    stream = io.StringIO()
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(TransportTests)
    result = unittest.TextTestRunner(stream=stream, verbosity=2).run(suite)
    if EVIDENCE is not None:
        m._save(EVIDENCE / 'suite.txt', stream.getvalue().encode())
        m._save(EVIDENCE / 'summary.json', dict(tests=result.testsRun, failures=len(result.failures),
                errors=len(result.errors), optimized=bool(sys.flags.optimize)))
    print(stream.getvalue(), flush=True)
    sys.exit(not result.wasSuccessful() or result.testsRun == 0)
