#!/usr/bin/env python3
"""Loopback Responses stand-in (WebSocket + plain HTTP SSE) for the #1038 TUI check.

Every response.create gets response.created; a warm-up (generate: false) then
completes with no output, any other request gets one short assistant message.
Logs one line per request: transport, generate flag, model (no headers/bodies).
"""
import asyncio
import json
import sys
import time

from websockets.asyncio.server import serve
from websockets.datastructures import Headers
from websockets.http11 import Response

PORT = int(sys.argv[1])
LOG = open(sys.argv[2], "a", buffering=1)
N = {"n": 0}
USAGE = {"input_tokens": 1200, "input_tokens_details": {"cached_tokens": 0},
         "output_tokens": 8, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 1208}


def events(req):
    N["n"] += 1
    rid = f"resp_mock_{N['n']}"
    warm = req.get("generate") is False
    out = [{"type": "response.created", "response": {"id": rid}}]
    if not warm:
        out.append({"type": "response.output_item.done", "item": {
            "type": "message", "role": "assistant", "id": f"msg_{N['n']}",
            "content": [{"type": "output_text", "text": f"Mock reply #{N['n']}."}]}})
    usage = dict(USAGE, output_tokens=0, total_tokens=1200) if warm else USAGE
    out.append({"type": "response.completed", "response": {"id": rid, "usage": usage}})
    return warm, out


def log(kind, req, warm):
    LOG.write(f"{time.strftime('%H:%M:%SZ', time.gmtime())} {kind} n={N['n']} "
              f"generate={req.get('generate')} warmup={warm} model={req.get('model')}\n")


async def handler(ws):
    async for raw in ws:
        try:
            req = json.loads(raw)
        except ValueError:
            continue
        if req.get("type") != "response.create":
            LOG.write(f"ws other type={req.get('type')}\n")
            continue
        warm, out = events(req)
        log("ws", req, warm)
        for ev in out:
            await ws.send(json.dumps(ev))


def process_request(connection, request):
    if request.headers.get("Upgrade", "").lower() == "websocket":
        return None
    LOG.write(f"http {request.path}\n")
    if request.path.endswith("/models"):
        body = json.dumps({"data": [], "object": "list"}).encode()
        return Response(200, "OK", Headers({"Content-Type": "application/json"}), body)
    return Response(404, "Not Found", Headers({"Content-Type": "text/plain"}), b"websocket only\n")


async def main():
    async with serve(handler, "127.0.0.1", PORT, process_request=process_request, max_size=None):
        await asyncio.Future()


asyncio.run(main())
