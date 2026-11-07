#!/usr/bin/env python3
"""PF-60-S10 provider stand-ins (plain HTTP on loopback, behind acc_mediator.py's TLS).

Dispatches by Host. Every answer is a short text with fixed, documented usage, so
the tester can recompute `/cost` from the packet:
- openrouter.ai        Chat Completions; charge in usage.cost (a JSON number)
- ai-gateway.vercel.sh Responses; charge in response.provider_metadata.gateway.gatewayCost
- api.corbanu.com      Chat Completions; charge in usage.corbanu_charge_usd (decimal text)
- llm.mylab.test       Chat Completions; no charge (a person-configured provider)
Logs host, path, model and the usage it answered with (no headers, no bodies).
"""
import argparse
import http.server
import json
import threading
import time

LOCK = threading.Lock()
STATE = {"omit_charge": False}  # set by POST /_standin {"omit_charge": true|false} (tester control)

OPENROUTER = {
    "z-ai/glm-5.3-flash": {"prompt_tokens": 2000, "completion_tokens": 40, "total_tokens": 2040,
                           "prompt_tokens_details": {"cached_tokens": 500},
                           "completion_tokens_details": {"reasoning_tokens": 12}, "cost": 0.00052},
    # Reasoning above output (#786): 60 reasoning tokens in 40 output tokens.
    "deepseek/deepseek-v4.1-flash": {"prompt_tokens": 1000, "completion_tokens": 40, "total_tokens": 1040,
                                     "prompt_tokens_details": {"cached_tokens": 0},
                                     "completion_tokens_details": {"reasoning_tokens": 60}, "cost": 0.00031},
    # No charge reported: a catalogued model, and one the catalogue doesn't list.
    "xiaomi/mimo-v2.6-pro": {"prompt_tokens": 1200, "completion_tokens": 30, "total_tokens": 1230},
    "standin/uncatalogued": {"prompt_tokens": 900, "completion_tokens": 15, "total_tokens": 915},
}
OPENROUTER_OTHER = {"prompt_tokens": 900, "completion_tokens": 15, "total_tokens": 915}
CHAT = {
    "api.corbanu.com": {"prompt_tokens": 1500, "completion_tokens": 30, "total_tokens": 1530,
                        "corbanu_charge_usd": "0.001850"},
    "llm.mylab.test": {"prompt_tokens": 800, "completion_tokens": 20, "total_tokens": 820},
}
VERCEL_USAGE = {"input_tokens": 3000, "input_tokens_details": {"cached_tokens": 1000},
                "output_tokens": 25, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 3025}
VERCEL_COST = "0.0007125"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--log", required=True)
    args = parser.parse_args()

    def log(**event):
        with LOCK, open(args.log, "a") as handle:
            handle.write(json.dumps(dict(t=round(time.time(), 3), **event), sort_keys=True) + "\n")

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_):
            pass

        def host(self):
            return (self.headers.get("host") or "").split(":")[0]

        def reply(self, status, body):
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(data)

        def sse(self, events, named=False):
            payload = ""
            for name, data in events:
                payload += ("event: %s\n" % name if named else "") + "data: %s\n\n" % json.dumps(data)
            if not named:
                payload += "data: [DONE]\n\n"
            data = payload.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            log(event="get", host=self.host(), path=self.path.split("?")[0],
                upgrade=self.headers.get("upgrade"))
            if (self.headers.get("upgrade") or "").lower() == "websocket":
                return self.reply(426, {"error": {"message": "upgrade required"}})
            return self.reply(404, {"error": {"message": "not found (stand-in)"}})

        def do_POST(self):
            raw = self.rfile.read(int(self.headers.get("content-length", 0) or 0))
            try:
                body = json.loads(raw or b"{}") if not self.headers.get("content-encoding") else {}
            except ValueError:
                body = {}
            host, path = self.host(), self.path.split("?")[0]
            model = body.get("model") if isinstance(body, dict) else None
            if path == "/_standin" and isinstance(body, dict) and isinstance(body.get("omit_charge"), bool):
                STATE["omit_charge"] = body["omit_charge"]
                log(event="control", host=host, omit_charge=STATE["omit_charge"])
                return self.reply(200, {"omit_charge": STATE["omit_charge"]})
            if host == "ai-gateway.vercel.sh" and path.endswith("/responses"):
                rid = "resp_standin_%d" % time.time_ns()
                item = {"type": "message", "role": "assistant", "id": "msg_1",
                        "content": [{"type": "output_text", "text": "OK"}]}
                response = {"id": rid, "model": model, "usage": VERCEL_USAGE}
                if not STATE["omit_charge"]:
                    response["provider_metadata"] = {"gateway": {"gatewayCost": VERCEL_COST}}
                log(event="answer", host=host, path=path, model=model, usage=VERCEL_USAGE,
                    gatewayCost=None if STATE["omit_charge"] else VERCEL_COST)
                return self.sse([("response.created", {"type": "response.created", "response": {"id": rid}}),
                                 ("response.output_item.done", {"type": "response.output_item.done", "item": item}),
                                 ("response.completed", {"type": "response.completed", "response": response})],
                                named=True)
            if path.endswith("/chat/completions") and host in ("openrouter.ai", "api.corbanu.com", "llm.mylab.test"):
                if host == "openrouter.ai":
                    usage = OPENROUTER.get(model, OPENROUTER_OTHER)
                else:
                    usage = CHAT[host]
                if STATE["omit_charge"]:
                    usage = {k: v for k, v in usage.items() if k not in ("cost", "corbanu_charge_usd")}
                base = {"id": "chatcmpl-standin-%d" % time.time_ns(), "object": "chat.completion.chunk",
                        "created": int(time.time()), "model": model}
                log(event="answer", host=host, path=path, model=model, usage=usage)
                return self.sse([
                    ("chunk", dict(base, choices=[{"index": 0, "delta": {"role": "assistant", "content": "OK"},
                                                   "finish_reason": None}])),
                    ("chunk", dict(base, choices=[{"index": 0, "delta": {}, "finish_reason": "stop"}])),
                    ("chunk", dict(base, choices=[], usage=usage)),
                ])
            log(event="unanswered", host=host, path=path, model=model)
            return self.reply(404, {"error": {"message": "not found (stand-in)"}})

    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    log(event="started", port=args.port)
    server.serve_forever()


if __name__ == "__main__":
    main()
