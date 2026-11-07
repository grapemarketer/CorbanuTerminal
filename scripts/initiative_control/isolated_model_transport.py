"""Trusted host library; callers supply UTF-8 text and optionally inline PNG bytes.

Not an isolation boundary against arbitrary Python in this owner process.
Import, construction, help and OFF do not inspect credentials or start processes.
"""
from dataclasses import dataclass, field
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
from types import MappingProxyType
import uuid
import zlib

BINARY_SHA = '4a8eba7b10199ea49aee42a720687b63cb4b1b2194c2ce164d1b9f6f88ee510e'
CATALOG_SHA = 'f25b81d476efdad4f37fcd424bed00940f754214fcaf3e00452a79e560eaf09c'
_CATALOG = Path(__file__).absolute().with_name('isolated-model-catalog.json')
_CA = Path('/private/etc/ssl/cert.pem')
_WALL, _OUTPUT, _INPUT = 90, 256 * 1024, 64 * 1024
_IMAGE_BYTES, _IMAGE_SIDE, _IMAGE_PIXELS = 4 * 1024 * 1024, 4096, 4 * 1024 * 1024
_IMAGE_NAME = 'observation.png'
_NEUTRAL = 'Process the supplied text and return text.'
_DISABLED = tuple('''hooks plugin_hooks shell_tool shell_snapshot unified_exec shell_zsh_fork
unified_exec_zsh_fork apps plugins tool_suggest recommended_plugins memories
external_agent_memory_import code_mode code_mode_only code_mode_host
code_mode_buffered_exec multi_agent multi_agent_v2 multi_agent_mode enable_fanout
standalone_web_search web_search_request web_search_cached search_tool tool_search
deferred_executor token_budget current_time_reminder sleep_tool image_generation
request_permissions_tool skill_search skill_mcp_dependency_install
executor_capability_discovery in_app_browser browser_use browser_use_external
computer_use remote_plugin plugin_sharing external_migration goals artifact
workspace_dependencies enable_request_compression responses_websockets
responses_websockets_v2 remote_compaction_v2 personality collaboration_modes
remote_control chronicle'''.split())
_FIXED = MappingProxyType({
    'model_provider': 'isolated-openai-transport', 'model_reasoning_effort': 'high',
    'approval_policy': 'never', 'model_reasoning_summary': 'none', 'project_doc_max_bytes': 0,
    'notify': (), 'instructions': _NEUTRAL, 'developer_instructions': '',
    'include_permissions_instructions': False, 'include_apps_instructions': False,
    'include_collaboration_mode_instructions': False, 'include_environment_context': False,
    'cli_auth_credentials_store': 'file', 'mcp_oauth_credentials_store': 'file',
    'web_search': 'disabled', 'tools.update_plan.enabled': False,
    'tools.experimental_request_user_input.enabled': False,
    'skills.bundled.enabled': False, 'skills.include_instructions': False,
    'orchestrator.skills.enabled': False, 'memories.use_memories': False,
    'memories.generate_memories': False, 'memories.dedicated_tools': False,
    'model_providers.isolated-openai-transport.name': 'Isolated OpenAI transport',
    'model_providers.isolated-openai-transport.wire_api': 'responses',
    'model_providers.isolated-openai-transport.requires_openai_auth': True,
    'model_providers.isolated-openai-transport.supports_websockets': False,
    'model_providers.isolated-openai-transport.request_max_retries': 0,
    'model_providers.isolated-openai-transport.stream_max_retries': 0,
})
_WARNINGS = frozenset(
    '`[features].' + feature + '` is deprecated because web search is enabled by default. '
    '(Set `web_search` to `"live"`, `"indexed"`, `"cached"`, or `"disabled"` at the top level '
    '(or under a profile) in config.toml if you want to override it.)'
    for feature in ('web_search_cached', 'web_search_request'))


def _packed(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def _digest(data):
    return hashlib.sha256(data).hexdigest()


def _png(data):
    """Validate bounded noninterlaced RGB/RGBA scanlines; no image transformation."""
    if type(data) is not bytes or not 57 <= len(data) <= _IMAGE_BYTES:
        raise ValueError('expected bounded inline PNG bytes')
    if data[:8] != b'\x89PNG\r\n\x1a\n':
        raise ValueError('invalid PNG signature')
    pos, chunks, payload_bytes, dimensions, payloads = 8, 0, 0, None, []
    while pos < len(data):
        chunks += 1
        if chunks > 1024 or len(data) - pos < 12:
            raise ValueError('invalid PNG chunk envelope')
        size, kind = struct.unpack_from('>I4s', data, pos)
        end = pos + 12 + size
        if end > len(data) or zlib.crc32(data[pos + 4:end - 4]) != int.from_bytes(data[end - 4:end], 'big'):
            raise ValueError('invalid PNG chunk length or CRC')
        if kind == b'IHDR' and chunks == 1 and size == 13:
            width, height, depth, color, compression, filtering, interlace = struct.unpack_from('>IIBBBBB', data, pos + 8)
            if (not 1 <= width <= _IMAGE_SIDE or not 1 <= height <= _IMAGE_SIDE
                    or width * height > _IMAGE_PIXELS):
                raise ValueError('PNG dimensions exceed bounds')
            if depth != 8 or color not in (2, 6) or (compression, filtering, interlace) != (0, 0, 0):
                raise ValueError('unsupported PNG format')
            dimensions = (width, height)
        elif kind == b'IDAT' and dimensions is not None:
            payload_bytes += size
            payloads.append(data[pos + 8:end - 4])
        elif kind == b'IEND' and dimensions is not None and payload_bytes and size == 0 and end == len(data):
            stride = 1 + width * (3 if color == 2 else 4)
            expected = height * stride
            decoder = zlib.decompressobj()
            try:
                raw = decoder.decompress(b''.join(payloads), expected + 1)
            except zlib.error as exc:
                raise ValueError('invalid PNG compressed pixels') from exc
            if (len(raw) != expected or not decoder.eof or decoder.unused_data
                    or decoder.unconsumed_tail or any(value > 4 for value in raw[::stride])):
                raise ValueError('invalid PNG scanlines or compressed stream')
            return dimensions
        else:
            raise ValueError('unsupported PNG chunk or order')
        pos = end
    raise ValueError('missing PNG end')


def _save(path, data):
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as out:
        out.write(data if isinstance(data, bytes) else _packed(data) + b'\n')


def _path(path, directory=False, private=False, root=False):
    if not path.is_absolute() or path != path.resolve(strict=True):
        raise ValueError('noncanonical path')
    info = path.lstat()
    if not (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode)):
        raise ValueError('wrong path type')
    mode = stat.S_IMODE(info.st_mode)
    if (info.st_uid not in ({0} if root else {os.getuid()}) or mode & 0o022
            or (private and mode != (0o700 if directory else 0o600))
            or (not directory and info.st_nlink != 1)):
        raise ValueError('unsafe ownership or permissions')
    for parent in path.parents:
        p = parent.stat()
        if p.st_uid not in (0, os.getuid()) or (p.st_mode & 0o022 and not p.st_mode & stat.S_ISVTX):
            raise ValueError('unsafe parent')
    return (info.st_dev, info.st_ino, info.st_mode, info.st_uid, 0 if directory else info.st_nlink)


def _fingerprint(path, **checks):
    identity = _path(path, **checks)
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), 'rb') as src:
        info = os.fstat(src.fileno())
        if identity != (info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_nlink):
            raise ValueError('path replaced')
        sha = hashlib.file_digest(src, 'sha256').hexdigest()
    if identity != _path(path, **checks):
        raise ValueError('path replaced')
    return (*identity, sha)


@dataclass(frozen=True)
class OwnerConfig:
    binary: Path
    auth: Path
    run_root: Path
    enabled: bool = False

    def __post_init__(self):
        if type(self.enabled) is not bool or any(
                not isinstance(p, Path) or not p.is_absolute()
                for p in (self.binary, self.auth, self.run_root)):
            raise ValueError('owner requires exact absolute paths and boolean enabled')


def _snapshots(owner):
    return {'binary': _fingerprint(owner.binary), 'auth': _fingerprint(owner.auth, private=True),
            'catalog': _fingerprint(_CATALOG), 'ca': _fingerprint(_CA, root=True)}


def _chatgpt_auth(path, expected_sha):
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), 'rb') as src:
        data = src.read(1024 * 1024 + 1)
    if len(data) > 1024 * 1024 or _digest(data) != expected_sha:
        raise ValueError('auth changed or oversized')
    auth = json.loads(data, object_pairs_hook=_object)
    tokens = auth.get('tokens')
    if (auth.get('auth_mode') not in (None, 'chatgpt') or auth.get('OPENAI_API_KEY')
            or any(auth.get(k) for k in ('agent_identity', 'personal_access_token', 'bedrock_api_key'))
            or not isinstance(tokens, dict)
            or any(type(tokens.get(k)) is not str or not tokens[k]
                   for k in ('id_token', 'access_token', 'refresh_token'))):
        raise ValueError('owner must provision native ChatGPT auth')


def _launch(owner, run, *, with_image=False):
    values = dict(_FIXED, model_catalog_json=str(run / 'model-only.json'))
    argv = [str(owner.binary), 'exec', '--strict-config', '--ephemeral', '--ignore-user-config',
            '--ignore-rules', '--skip-git-repo-check', '--json', '-s', 'read-only',
            '-C', str(run / 'cwd'), '-m', 'gpt-6-astra']
    for key, value in values.items():
        argv += ['-c', key + '=' + json.dumps(value)]
    for feature in _DISABLED:
        argv += ['--disable', feature]
    if with_image:
        argv += ['--image', str(run / _IMAGE_NAME), '--']
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(run / 'home'),
           'CORBANU_HOME': str(run / 'home'), 'CODEX_HOME': str(run / 'home'),
           'TMPDIR': str(run / 'tmp'), 'CODEX_EXEC_SERVER_URL': 'none', 'SSL_CERT_FILE': str(_CA)}
    return ('/usr/bin/sandbox-exec', '-p', _guard(owner, run, with_image=with_image), *argv, '-'), env


def _guard(owner, run, *, with_image=False):
    q = lambda p: json.dumps(str(p))
    return '\n'.join([
        '(version 1)', '(deny default)', '(allow signal (target self))', '(allow sysctl-read)',
        '(allow mach-lookup (global-name "com.apple.system.opendirectoryd.libinfo") '
        '(global-name "com.apple.logd") (global-name "com.apple.system.logger"))',
        '(allow system-mac-syscall (mac-policy-name "vnguard"))',
        '(allow system-mac-syscall (require-all (mac-policy-name "Sandbox") '
        '(mac-syscall-number 2 67)))',
        '(allow file-read* file-test-existence (literal "/"))',
        '(allow system-fcntl (fcntl-command F_ADDFILESIGS_RETURN F_CHECK_LV F_GETPATH))',
        '(allow file-map-executable (subpath "/System/Library") '
        '(subpath "/System/Volumes/Preboot/Cryptexes/OS") (subpath "/usr/lib"))',
        f'(allow file-map-executable (literal {q(owner.binary)}))',
        '(deny mach-lookup (global-name "com.apple.securityd") '
        '(global-name "com.apple.securityd.xpc") (global-name "com.apple.secd"))',
        '(allow file-read-metadata)',
        '(allow file-read* (subpath "/System/Library") '
        '(subpath "/System/Volumes/Preboot/Cryptexes/OS") (subpath "/usr/lib") '
        '(subpath "/usr/share") (literal "/private/etc/localtime") '
        '(literal "/dev/null") (literal "/dev/urandom") (literal "/dev/random"))',
        '(allow file-read* ' + ' '.join(f'(literal {q(p)})' for p in
            (owner.binary, owner.auth, _CA, run / 'model-only.json', run / 'home/auth.json')) + ')',
        f'(allow file-read* (subpath {q(run / "tmp")}))',
        '(allow file-read* ' + ' '.join(f'(literal {q(run / n)})' for n in ('', 'home', 'cwd')) + ')',
        f'(allow file-read* (subpath {q(run / "home/tmp")}) '
        f'(literal {q(run / "home/installation_id")}))',
        '(allow file-write* ' + ' '.join(f'(subpath {q(run / n)})' for n in ('home', 'tmp')) + ')',
        f'(deny file-write* (literal {q(run / "home/auth.json")}) (literal {q(owner.auth)}))',
        '(deny file-read-data (regex #"/Keychains(/|$)"))',
        f'(allow process-exec (literal {q(owner.binary)}))',
        '(allow network-outbound (remote ip "*:443") (literal "/private/var/run/mDNSResponder"))',
    ] + ([f'(allow file-read* (literal {q(run / _IMAGE_NAME)}))'] if with_image else []))


def _bounded(argv, env, run, packet):
    started = time.monotonic()
    sel = selectors.DefaultSelector()
    try:
        proc = subprocess.Popen(argv, cwd=run, env=env, stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                start_new_session=True, close_fds=True)
    except BaseException:
        sel.close()
        raise
    output = {'stdout': bytearray(), 'stderr': bytearray()}
    reason, sent, total = 'exit', 0, 0
    try:
        for name in ('stdin', 'stdout', 'stderr'):
            pipe = getattr(proc, name)
            os.set_blocking(pipe.fileno(), False)
            sel.register(pipe, selectors.EVENT_WRITE if name == 'stdin' else selectors.EVENT_READ, name)
        while sel.get_map() or proc.poll() is None:
            remaining_time = _WALL - (time.monotonic() - started)
            if remaining_time <= 0:
                reason = 'timeout'
                break
            for key, _ in sel.select(min(.05, remaining_time)):
                try:
                    if key.data == 'stdin':
                        sent += os.write(key.fd, packet[sent:sent + 4096]) if sent < len(packet) else 0
                        if sent == len(packet):
                            sel.unregister(key.fileobj)
                            key.fileobj.close()
                        continue
                    data = os.read(key.fd, min(16384, _OUTPUT - total + 1))
                except BlockingIOError:
                    continue
                except BrokenPipeError:
                    sel.unregister(key.fileobj)
                    key.fileobj.close()
                    reason = 'input_closed'
                    continue
                if not data:
                    sel.unregister(key.fileobj)
                    continue
                output[key.data].extend(data[:_OUTPUT - total])
                total += len(data)
                if total > _OUTPUT:
                    reason = 'output_limit'
                    break
            if reason == 'output_limit':
                break
    finally:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except PermissionError:
            if proc.poll() is None:
                proc.kill()
        finally:
            proc.wait(timeout=2)
            sel.close()
            for name in ('stdin', 'stdout', 'stderr'):
                getattr(proc, name).close()
        for name, data in output.items():
            _save(run / (name + '.raw'), bytes(data))
    try:
        os.killpg(proc.pid, 0)
        gone = False
    except ProcessLookupError:
        gone = True
    except PermissionError:
        groups = subprocess.run(['/bin/ps', '-axo', 'pgid='], env={}, capture_output=True,
                                timeout=2, check=True).stdout.split()
        gone = str(proc.pid).encode() not in groups
    return output, dict(reason=reason, exit_code=proc.returncode, owned_group_gone=gone,
                        elapsed=time.monotonic() - started, input_bytes_sent=sent)


def _object(pairs):
    value = dict(pairs)
    if len(value) != len(pairs):
        raise ValueError('duplicate JSON key')
    return value


def _validate(raw):
    session, phase, warnings, messages, ids = None, 0, 0, [], set()
    # JSONL uses LF framing; Unicode separators inside JSON strings are data.
    lines = raw.split(b'\n')
    if lines[-1] == b'':
        lines.pop()
    for line in lines:
        event = json.loads(line.decode('utf-8'), object_pairs_hook=_object)
        kind = event['type']
        if kind == 'thread.started' and phase == 0 and set(event) == {'type', 'thread_id'}:
            session = str(uuid.UUID(event['thread_id']))
            if session != event['thread_id']:
                raise ValueError('invalid session')
            phase = 1
        elif kind == 'turn.started' and phase == 1 and set(event) == {'type'}:
            phase = 2
        elif kind == 'turn.completed' and phase == 2 and set(event) == {'type', 'usage'}:
            usage = event['usage']
            allowed = {'input_tokens', 'cached_input_tokens', 'output_tokens',
                       'cache_write_input_tokens', 'reasoning_output_tokens'}
            if (usage is not None and (not isinstance(usage, dict) or not {'input_tokens', 'cached_input_tokens', 'output_tokens'} <= usage.keys()
                    or not usage.keys() <= allowed or any(type(n) is not int or n < 0 for n in usage.values()))):
                raise ValueError('invalid usage')
            phase = 3
        elif kind == 'item.completed' and phase < 3 and set(event) == {'type', 'item'}:
            item = event['item']
            identifier, category = item['id'], item['type']
            if type(identifier) is not str or not identifier or identifier in ids:
                raise ValueError('invalid item identity')
            ids.add(identifier)
            if category == 'error' and set(item) == {'id', 'type', 'message'} and item['message'] in _WARNINGS:
                warnings += 1
            elif (category in ('reasoning', 'agent_message') and phase == 2
                  and set(item) == {'id', 'type', 'text'} and type(item['text']) is str):
                item['text'].encode('utf-8')
                if category == 'agent_message':
                    messages.append(item['text'])
            else:
                raise ValueError('unrecognized or error item')
        else:
            raise ValueError('unrecognized, error or out-of-order event')
    if phase != 3 or not messages or not any(messages):
        raise ValueError('incomplete text turn')
    return '\n'.join(messages), session, warnings


@dataclass(frozen=True)
class Transport:
    owner: OwnerConfig
    _pins: dict = field(default_factory=dict, init=False, repr=False)
    _sessions: set = field(default_factory=set, init=False, repr=False)
    _lock: object = field(default_factory=threading.Lock, init=False, repr=False)

    def execute(self, text, *, image_png=None):
        """Return a safe receipt; optional image is bytes only, never a path/URL."""
        if not self.owner.enabled:
            return MappingProxyType({'status': 'off', 'text': None})
        if type(text) is not str or len(text) > _INPUT:
            raise ValueError('expected bounded UTF-8 text')
        packet = text.encode('utf-8')
        if len(packet) > _INPUT:
            raise ValueError('input exceeds 64KiB')
        dimensions = _png(image_png) if image_png is not None else None
        if dimensions and ',' in str(self.owner.run_root):
            raise ValueError('image owner root cannot contain CLI image delimiter')
        if not self._lock.acquire(blocking=False):
            raise ValueError('transport busy')
        try:
            return self._execute(packet, image_png, dimensions)
        finally:
            self._lock.release()

    def _execute(self, packet, image_png, dimensions):
        owner = self.owner
        root_id = _path(owner.run_root, directory=True, private=True)
        run = Path(tempfile.mkdtemp(prefix='attempt-', dir=owner.run_root))
        receipt = dict(attempt_id=run.name, status='failed', text=None, reason='preflight',
                       model='gpt-6-astra', effort='high', provider='isolated-openai-transport')
        try:
            if sys.platform != 'darwin':
                raise ValueError('macOS required')
            before = _snapshots(owner)
            if before['binary'][-1] != BINARY_SHA or before['catalog'][-1] != CATALOG_SHA:
                raise ValueError('package or catalog mismatch')
            if not os.access(owner.binary, os.X_OK):
                raise ValueError('package is not executable')
            _chatgpt_auth(owner.auth, before['auth'][-1])
            pins = dict(before, root=root_id)
            if self._pins and self._pins != pins:
                raise ValueError('owner inputs changed since first attempt')
            self._pins.update(pins)
            for name in ('home', 'cwd', 'tmp'):
                (run / name).mkdir(mode=0o700)
            catalog = _CATALOG.read_bytes()
            if _digest(catalog) != CATALOG_SHA:
                raise ValueError('catalog changed')
            _save(run / 'model-only.json', catalog)
            (run / 'home/auth.json').symlink_to(owner.auth)
            image_pin = None
            if image_png is not None:
                _save(run / _IMAGE_NAME, image_png)
                image_pin = _fingerprint(run / _IMAGE_NAME, private=True)
                if image_pin[-1] != _digest(image_png):
                    raise ValueError('image changed before launch')
                receipt.update(image_sha256=image_pin[-1], image_bytes=len(image_png),
                               image_width=dimensions[0], image_height=dimensions[1],
                               image_observation='supplied_not_attested')
                _save(run / 'image.json', dict(identity=image_pin, **{
                    k: v for k, v in receipt.items() if k.startswith('image_')}))
            argv, env = _launch(owner, run, with_image=True) if image_pin else _launch(owner, run)
            _save(run / 'launch.json', dict(argv=argv, environment=env, pins=pins,
                  argv_sha256=_digest(_packed(argv)), policy_sha256=_digest(argv[2].encode()),
                  profile_sha256=_digest(_packed({'fixed': dict(_FIXED), 'disabled': _DISABLED})),
                  input_sha256=_digest(packet), input_bytes=len(packet), wall_seconds=_WALL, output_bytes=_OUTPUT))
            receipt['reason'] = 'launch'
            if image_pin and _fingerprint(run / _IMAGE_NAME, private=True) != image_pin:
                raise ValueError('image changed before launch')
            raw, process = _bounded(argv, env, run, packet)
            receipt.update(process)
            try:
                unchanged = (_snapshots(owner) == before and _path(owner.run_root, directory=True, private=True) == root_id
                             and _fingerprint(run / 'model-only.json')[-1] == CATALOG_SHA
                             and (run / 'home/auth.json').is_symlink()
                             and os.readlink(run / 'home/auth.json') == str(owner.auth)
                             and (image_pin is None or _fingerprint(run / _IMAGE_NAME, private=True) == image_pin))
            except (OSError, ValueError):
                unchanged = False
            receipt['inputs_unchanged'] = unchanged
            if not unchanged:
                receipt['reason'] = 'inputs_changed'
            if not unchanged or process['reason'] != 'exit' or process['exit_code'] != 0 or not process['owned_group_gone']:
                raise ValueError('process or immutability failure')
            receipt['reason'] = 'events'
            answer, session, warnings = _validate(raw['stdout'])
            if session in self._sessions:
                raise ValueError('reused native session')
            self._sessions.add(session)
            receipt.update(status='ok', reason='validated', text=answer,
                           session_id=session, known_warning_count=warnings)
        except Exception as error:
            _save(run / 'failure.json', {'type': type(error).__name__, 'detail': str(error)})
        finally:
            for name in ('stdout.raw', 'stderr.raw'):
                if not (run / name).exists():
                    _save(run / name, b'')
            _save(run / 'receipt.json', receipt)
        return MappingProxyType(receipt)


if __name__ == '__main__':
    import argparse
    argparse.ArgumentParser(description='Owner-only library; no live CLI. Use Transport.execute(text).').parse_args()
