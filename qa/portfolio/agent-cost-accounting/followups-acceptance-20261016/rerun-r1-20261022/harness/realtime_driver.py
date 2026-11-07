#!/usr/bin/env python3
"""PF-60-S09 realtime driver: one real realtime session through the app server.

Starts `<corbanu> app-server` on stdio in the caller's environment (a disposable
home with an API-key login), opens a thread, starts realtime (text output by
default), sends "Reply with the single word OK." with `appendSpeech` (which asks
for a response; `appendText` only adds an item), waits for the reply, stops, and
exits. It prints the JSON-RPC methods it saw and the reply text; it never prints
credentials (it never reads any: the server uses the home's login).

usage: realtime_driver.py <corbanu> <workspace> [--transport websocket|webrtc --sdp-file F]
                          [--modality text|audio] [--version v1|v2|v3] [--model M]
                          [--config KEY=VALUE ...]

acc-tart1 (2026-10-20): --config passes `-c KEY=VALUE` to `corbanu app-server` (the harness turns the
realtime feature on and keeps the profile's file credential store and log folder this way).
"""
import argparse
import json
import queue
import subprocess
import sys
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument("bin")
parser.add_argument("workspace")
parser.add_argument("--transport", default="websocket")
parser.add_argument("--sdp-file")
parser.add_argument("--model")
parser.add_argument("--version")
parser.add_argument("--modality", default="text")
parser.add_argument("--text", default="Reply with the single word OK.")
parser.add_argument("--timeout", type=float, default=60)
parser.add_argument("--config", action="append", default=[])
args = parser.parse_args()

proc = subprocess.Popen(
    [args.bin, "-C", args.workspace, "app-server"] + [a for kv in args.config for a in ("-c", kv)],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    text=True,
    bufsize=1,
)
inbox: "queue.Queue[dict]" = queue.Queue()


def reader():
    for line in proc.stdout:
        line = line.strip()
        if line:
            try:
                inbox.put(json.loads(line))
            except ValueError:
                print("non-JSON line from server", file=sys.stderr)


threading.Thread(target=reader, daemon=True).start()
next_id = 0
seen = []


def send(method, params=None, notify=False):
    global next_id
    message = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        message["params"] = params
    if not notify:
        next_id += 1
        message["id"] = next_id
    proc.stdin.write(json.dumps(message) + "\n")
    proc.stdin.flush()
    return None if notify else next_id


def wait(predicate, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            message = inbox.get(timeout=max(0.1, deadline - time.time()))
        except queue.Empty:
            break
        method = message.get("method")
        if method:
            seen.append(method)
        if predicate(message):
            return message
    return None


def call(method, params, timeout=30):
    request = send(method, params)
    reply = wait(lambda m: m.get("id") == request, timeout)
    if reply is None:
        sys.exit(f"no reply to {method}")
    if "error" in reply:
        sys.exit(f"{method} failed: {json.dumps(reply['error'])[:500]}")
    return reply.get("result", {})


call("initialize", {
    "clientInfo": {"name": "pf60-s09-realtime-driver", "title": None, "version": "0"},
    "capabilities": {"experimentalApi": True},
})
send("initialized", notify=True)
thread = call("thread/start", {})["thread"]["id"]
if args.transport not in ("websocket", "webrtc"):
    sys.exit(f"unknown transport {args.transport!r}")
transport = {"type": "websocket"}
if args.transport == "webrtc":
    transport = {"type": "webrtc", "sdp": open(args.sdp_file).read()}
start = {
    "threadId": thread,
    "outputModality": args.modality,
    "transport": transport,
    "includeStartupContext": False,
}
if args.model:
    start["model"] = args.model
if args.version:
    start["version"] = args.version
call("thread/realtime/start", start, timeout=60)
started = wait(lambda m: m.get("method") in ("thread/realtime/started", "thread/realtime/error", "thread/realtime/closed"), 30)
print("start:", started and started.get("method"))
if started and started.get("method") == "thread/realtime/error":
    print("error:", json.dumps(started.get("params", {}))[:400])
if started and started.get("method") == "thread/realtime/started":
    call("thread/realtime/appendSpeech", {"threadId": thread, "text": args.text})
    reply = []

    def done(message):
        method = message.get("method", "")
        params = message.get("params", {})
        if method == "thread/realtime/transcript/delta":
            reply.append(params.get("delta", ""))
        if method == "thread/realtime/itemAdded":
            item = params.get("item", {})
            text = json.dumps(item)[:300]
            print("item:", text)
            if '"type": "response.done"' in text or '"response.done"' in text:
                return True
        return method in ("thread/realtime/transcript/done", "thread/realtime/error", "thread/realtime/closed")

    end = wait(done, args.timeout)
    print("reply end:", end and end.get("method"))
    print("reply text:", "".join(reply)[:200] or (end or {}).get("params", {}).get("text", "")[:200])
    call("thread/realtime/stop", {"threadId": thread})
    wait(lambda m: m.get("method") == "thread/realtime/closed", 10)
print("thread:", thread)
print("methods seen:", sorted(set(seen)))
proc.stdin.close()
try:
    proc.wait(timeout=15)
except subprocess.TimeoutExpired:
    proc.terminate()
