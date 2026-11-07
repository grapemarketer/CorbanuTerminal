#!/usr/bin/env python3
"""ChatGPT sign-in and Responses stand-in for PF-60-S09 AC1, adapted from PF-84-S06 slice B's mock
(qa/provider-auth/pf-84/s06/scripts/mock_openai.py). Served over TLS (--cert/--key) behind
`proxy.py`, which tunnels CONNECT auth.openai.com:443 and chatgpt.com:443 here, so the client
uses its built-in ChatGPT routes unchanged. Fake tokens only.

Original notes:

Device-code sign-in (`/api/accounts/deviceauth/*`, `/oauth/token` code exchange) signs in
the user last set with POST /_next_user {"user": "<name>", "ttl": <access token seconds, default 60>}. Every token is fake and
encodes only `<user>` and a generation number; the access token expires 60 s after it is
issued, so the first request refreshes it. A refresh must present the newest refresh
token of that user (rotation; an older one gets `refresh_token_reused`); users whose name
starts with `bad` get an already expired access token and `refresh_token_expired`. An
expired access token gets 401 from */responses. POST */responses answers with the
user and token generation it was authorized as. The log (--log) records users,
generations and events only.
"""
import argparse
import base64
import http.server
import json
import threading
import time

LOCK = threading.Lock()
STATE = {"next_user": "work", "gen": {}}


def b64(data):
    return base64.urlsafe_b64encode(json.dumps(data).encode()).rstrip(b"=").decode()


def jwt(payload):
    return "%s.%s.%s" % (b64({"alg": "none", "typ": "JWT"}), b64(payload), "c2ln")


def claims(token):
    try:
        part = token.split(".")[1]
        return json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))
    except Exception:  # noqa: BLE001 - any non-JWT is an API key or garbage
        return None


def issue(user, gen, access_ttl):
    now = int(time.time())
    id_token = jwt({
        "sub": "user-%s" % user,
        "email": "%s@example.com" % user.split("-")[0],
        "https://api.openai.com/auth": {
            "chatgpt_user_id": "chatgpt-user-%s" % user.split("-")[0],
            "chatgpt_account_id": "workspace-%s" % user,
            "chatgpt_plan_type": "plus",
        },
    })
    access = jwt({"user": user, "gen": gen, "exp": now + access_ttl})
    return {"id_token": id_token, "access_token": access, "refresh_token": "rt-%s-%d" % (user, gen)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--log", required=True)
    parser.add_argument("--cert")
    parser.add_argument("--key")
    args = parser.parse_args()

    def log(**event):
        with LOCK, open(args.log, "a") as handle:
            handle.write(json.dumps(dict(t=round(time.time(), 3), **event)) + "\n")

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_):
            pass

        def reply(self, status, body=None, ctype="application/json"):
            data = b"" if body is None else (body if isinstance(body, bytes) else json.dumps(body).encode())
            self.send_response(status)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def who(self):
            auth = self.headers.get("authorization", "")
            token = auth[7:] if auth.lower().startswith("bearer ") else ""
            payload = claims(token)
            if payload and "user" in payload:
                return "%s (token gen %s)" % (payload["user"], payload["gen"])
            if token.startswith("sk-fake-"):
                return "api key %s" % token[len("sk-fake-"):]
            return "unknown" if token else "none"

        def do_GET(self):
            log(event="get", path=self.path.split("?")[0], who=self.who(),
                upgrade=self.headers.get("upgrade"))
            # No WebSocket here: the client falls back to HTTP for Responses.
            if (self.headers.get("upgrade") or "").lower() == "websocket":
                return self.reply(426, {"error": {"message": "upgrade required"}})
            self.reply(404)

        def do_POST(self):
            raw = self.rfile.read(int(self.headers.get("content-length", 0)) or 0)
            ctype = self.headers.get("content-type", "")
            body = {}
            # Responses bodies may be compressed and are not needed.
            if not self.headers.get("content-encoding"):
                try:
                    if "json" in ctype:
                        body = json.loads(raw or b"{}")
                    else:
                        body = dict(pair.split("=", 1) for pair in raw.decode().split("&") if "=" in pair)
                except (ValueError, UnicodeDecodeError):
                    body = {}
            path = self.path.split("?")[0]
            if path == "/_script":
                STATE["script"] = list(body.get("replies", []))
                return self.reply(200, {})
            if path.endswith("/images/generations"):
                log(event="images", who=self.who(), host=self.headers.get("host"), path=path)
                # A 1x1 white PNG.
                png = ("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4"
                       "//8/AwAI/AL+XJ/PAAAAAElFTkSuQmCC")
                return self.reply(200, {
                    "created": int(time.time()),
                    "data": [{"b64_json": png}],
                    "usage": {"input_tokens": 48, "output_tokens": 229, "total_tokens": 277,
                              "input_tokens_details": {"text_tokens": 48, "image_tokens": 0}},
                })
            if path == "/_next_user":
                STATE["next_user"] = body["user"]
                STATE["ttl"] = int(body.get("ttl", 60))
                return self.reply(200, {})
            if path.endswith("/deviceauth/usercode"):
                return self.reply(200, {"device_auth_id": "dev-1", "user_code": "MOCK-0001", "interval": "0"})
            if path.endswith("/deviceauth/token"):
                return self.reply(200, {"authorization_code": "authcode-%s" % STATE["next_user"],
                                        "code_challenge": "c", "code_verifier": "v"})
            if path == "/oauth/revoke":
                token = body.get("token", "")
                log(event="revoke", user=token.split("-")[1] if token.startswith("rt-") else "?")
                return self.reply(200, {})
            if path == "/oauth/token":
                if body.get("grant_type") == "authorization_code":
                    user = body.get("code", "").replace("authcode-", "", 1)
                    with LOCK:
                        STATE["gen"][user] = 1
                    log(event="sign-in", user=user)
                    # A `bad` user's access token is already expired.
                    ttl = -60 if user.startswith("bad") else STATE.get("ttl", 60)
                    return self.reply(200, issue(user, 1, access_ttl=ttl))
                rt = body.get("refresh_token", "")
                user, gen = rt[3:].rsplit("-", 1) if rt.startswith("rt-") else ("?", "0")
                if user.startswith("bad"):
                    log(event="refresh-rejected", user=user, reason="expired")
                    return self.reply(401, {"error": {"code": "refresh_token_expired",
                                                      "message": "refresh token expired"}})
                with LOCK:
                    current = STATE["gen"].get(user)
                    if current is None or int(gen) != current:
                        log(event="refresh-rejected", user=user, reason="reused", gen=int(gen))
                        return self.reply(401, {"error": {"code": "refresh_token_reused",
                                                          "message": "refresh token reused"}})
                    STATE["gen"][user] = current + 1
                log(event="refresh", user=user, from_gen=current, to_gen=current + 1)
                return self.reply(200, issue(user, current + 1, access_ttl=3600))
            if path.endswith("/responses"):
                who = self.who()
                token = self.headers.get("authorization", "")[7:]
                payload = claims(token)
                if payload and payload.get("exp", 0) < time.time():
                    log(event="responses-rejected", who=who, reason="expired access token")
                    return self.reply(401, {"error": {"code": "token_expired",
                                                      "message": "access token expired"}})
                script = STATE.setdefault("script", [])
                kind = script.pop(0) if script else "text"
                log(event="responses", who=who, host=self.headers.get("host"), path=path,
                    model=body.get("model"), service_tier=body.get("service_tier"), reply=kind)
                rid = "resp_mock_%d" % int(time.time() * 1000)
                if kind == "imagegen":
                    item = {"type": "function_call", "call_id": "call_img_1", "namespace": "image_gen",
                            "name": "imagegen",
                            "arguments": json.dumps({"prompt": "a plain red square on white"})}
                else:
                    item = {"type": "message", "role": "assistant", "id": "msg_1",
                            "content": [{"type": "output_text", "text": "openai-mock: authorized as %s" % who}]}
                usage = {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 0},
                         "output_tokens": 7, "output_tokens_details": {"reasoning_tokens": 0},
                         "total_tokens": 107}
                events = [("response.created", {"type": "response.created", "response": {"id": rid}}),
                          ("response.output_item.done", {"type": "response.output_item.done", "item": item}),
                          ("response.completed", {"type": "response.completed",
                                                  "response": {"id": rid, "usage": usage}})]
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Connection", "close")
                self.end_headers()
                for name, data in events:
                    self.wfile.write(("event: %s\ndata: %s\n\n" % (name, json.dumps(data))).encode())
                    self.wfile.flush()
                return None
            return self.reply(404)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    if args.cert:
        import ssl
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(args.cert, args.key)
        context.set_alpn_protocols(["http/1.1"])
        server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
