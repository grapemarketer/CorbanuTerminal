"""PF-60 follow-up (acc-tart1) inference mediator, from PF-60-S10's (runs as the coordinator account, never as an executor).

Accepts HTTP CONNECT only for the listed hosts on port 443, on loopback. Terminates TLS
with a campaign-local synthetic CA (one leaf naming every listed host), then either
- forwards allowlisted paths to the real provider, injecting the transport credential
  the executor never sees (real hosts), relaying WebSocket upgrades byte for byte, or
- forwards the request as plain HTTP to a local stand-in (stand-in hosts).
Logs host/method/path/status/model, and the provider-reported usage object of real
HTTP responses: no headers, no request bodies, no key.

acc-tart1 additions (2026-10-20, Tart guest):
- OpenAI realtime (/v1/realtime, WebSocket upgrades only) through CONNECT, key injected,
  relayed byte for byte. Upstream connections use --pin HOST=IP (api.openai.com's address).
- --realtime-port (direct TLS listener for a client that ignores HTTPS_PROXY) exists but is
  not enabled in this run: the shakedown showed the realtime client uses the proxy.
- --realtime-cap: realtime WebSocket sessions allowed in the whole run (owner's D16 limit).
- The usage object of realtime `response.done` events is logged (server frames only).
- A key file that doesn't exist is not loaded; that host then answers 503.

acct-rerun changes (2026-10-22): no real key on the guest. Real hosts are sent as plain HTTP (no
Authorization) to a host-side key relay through one SSH reverse tunnel on the guest's loopback
(--relay NAME=PORT); the relay adds the key and the TLS to the real host. A route without a relay answers 503.
"""
import argparse
import json
import socket
import socketserver
import ssl
import threading
import time
import zlib

REAL = {
    "api.z.ai": ("zai", ("/api/paas/v4/", "/api/coding/paas/v4/", "/api/anthropic/")),
    "api.openai.com": ("openai", ("/v1/responses", "/v1/images/generations", "/v1/images/edits",
                                  "/v1/models", "/v1/realtime")),
    "api.moonshot.ai": ("kimi", ("/v1/chat/completions", "/v1/models")),
}
STANDIN = {
    "auth.openai.com": 18601, "chatgpt.com": 18601, "ab.chatgpt.com": 18601,
    "openrouter.ai": 18602, "ai-gateway.vercel.sh": 18602, "api.corbanu.com": 18602,
    "llm.mylab.test": 18602,
}
DROP = {"authorization", "x-api-key", "proxy-authorization", "proxy-connection",
        "connection", "keep-alive", "transfer-encoding", "content-length", "host", "te",
        "upgrade"}
LOCK = threading.Lock()
SPEND = {"kimi_posts": 0, "realtime_sessions": 0}


def log(path, row):
    row["ts"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    row["time_ns"] = time.time_ns()
    with LOCK, open(path, "a") as out:
        out.write(json.dumps(row, sort_keys=True) + "\n")


def read_head(stream):
    lines = []
    while True:
        line = stream.readline(65537)
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            return lines
        lines.append(line.decode("latin-1").rstrip("\r\n"))
        if len(lines) > 200:
            raise ValueError("too many headers")


def read_body(stream, headers):
    if headers.get("transfer-encoding", "").lower() == "chunked":
        body = bytearray()
        while True:
            size = int(stream.readline().split(b";")[0].strip(), 16)
            if size == 0:
                while stream.readline() not in (b"\r\n", b"\n", b""):
                    pass
                return bytes(body)
            body += stream.read(size)
            stream.readline()
    length = int(headers.get("content-length", "0") or 0)
    return stream.read(length) if length else b""


def summarize(body):
    try:
        data = json.loads(body.decode("utf-8"))
    except Exception:
        return {}
    if not isinstance(data, dict):
        return {}
    return {"model": data.get("model"), "stream": data.get("stream"),
            "service_tier": data.get("service_tier")}


def usage_from(tail):
    """The last JSON object in an SSE/JSON response tail that carries `usage`."""
    found = None
    for line in tail.decode("utf-8", "replace").splitlines():
        line = line.strip()
        if line.startswith("data:"):
            line = line[5:].strip()
        if '"usage"' not in line or not line.startswith("{"):
            continue
        try:
            obj = json.loads(line)
        except ValueError:
            continue
        usage = obj.get("usage") or (obj.get("response") or {}).get("usage")
        if usage is not None:
            found = usage
    return found


def pipe(src, dst, tap=None):
    try:
        while True:
            data = src.recv(65536)
            if not data:
                break
            dst.sendall(data)
            if tap is not None:
                tap.feed(data)
    except Exception:
        pass
    finally:
        for sock in (src, dst):
            try:
                sock.shutdown(socket.SHUT_RDWR)
            except Exception:
                pass


class FrameTap:
    """Best-effort parser of server->client WebSocket frames; logs realtime usage reports."""

    def __init__(self, cfg, conn_id):
        self.cfg, self.conn_id, self.buf, self.parts, self.inflate = cfg, conn_id, b"", [], None
        self.compressed = False

    def feed(self, data):
        try:
            self.buf += data
            while len(self.buf) >= 2:
                b0, b1 = self.buf[0], self.buf[1]
                n, pos = b1 & 0x7F, 2
                if n == 126:
                    if len(self.buf) < 4:
                        return
                    n, pos = int.from_bytes(self.buf[2:4], "big"), 4
                elif n == 127:
                    if len(self.buf) < 10:
                        return
                    n, pos = int.from_bytes(self.buf[2:10], "big"), 10
                if b1 & 0x80:
                    pos += 4  # masked (not expected from a server)
                if len(self.buf) < pos + n:
                    return
                payload, self.buf = self.buf[pos:pos + n], self.buf[pos + n:]
                opcode = b0 & 0x0F
                if opcode in (1, 2):
                    self.parts, self.compressed = [payload], bool(b0 & 0x40)
                elif opcode == 0:
                    self.parts.append(payload)
                else:
                    continue
                if b0 & 0x80:
                    self.message(b"".join(self.parts))
                    self.parts = []
        except Exception as exc:  # never disturb the relay
            log(self.cfg.log, {"event": "ws_tap_error", "conn": self.conn_id, "error": repr(exc)[:200]})
            self.buf = b""

    def message(self, data):
        if self.compressed:
            if self.inflate is None:
                self.inflate = zlib.decompressobj(-15)
            data = self.inflate.decompress(data + b"\x00\x00\xff\xff")
        if b"response.done" not in data:
            return
        try:
            obj = json.loads(data.decode("utf-8"))
        except ValueError:
            return
        if obj.get("type") == "response.done":
            response = obj.get("response") or {}
            log(self.cfg.log, {"event": "realtime_response_done", "conn": self.conn_id,
                               "status": response.get("status"), "usage": response.get("usage")})


def pin_target(cfg, host):
    """--pin HOST=IP[:PORT]; unpinned hosts resolve normally on port 443."""
    ip, _, port = cfg.pins.get(host, host).partition(":")
    return ip, int(port or 443)


def realtime_admit(cfg, conn_id, path):
    with LOCK:
        SPEND["realtime_sessions"] += 1
        n = SPEND["realtime_sessions"]
    if n > cfg.realtime_cap:
        log(cfg.log, {"event": "denied_realtime_cap", "conn": conn_id, "path": path[:200],
                      "count": n, "cap": cfg.realtime_cap})
        return False
    return True


class RealtimeHandler(socketserver.BaseRequestHandler):
    """127.0.0.1:443: TLS for api.openai.com, GET /v1/realtime WebSocket upgrades only."""

    def handle(self):
        cfg = self.server.cfg
        conn_id = "rt-%d-%d" % (time.time_ns(), self.client_address[1])
        host = "api.openai.com"
        if not self.client_address[0].startswith("127."):
            log(cfg.log, {"event": "denied_non_loopback", "conn": conn_id, "peer": self.client_address[0]})
            return
        try:
            tls = cfg.server_ctx.wrap_socket(self.request, server_side=True)
        except Exception as exc:
            log(cfg.log, {"event": "client_tls_error", "conn": conn_id, "host": host + " (direct)",
                          "error": repr(exc)[:200]})
            return
        try:
            reader = tls.makefile("rb")
            head = read_head(reader)
            if not head:
                return
            method, path, _ = head[0].split(" ", 2)
            headers = {}
            for line in head[1:]:
                key, _, value = line.partition(":")
                headers[key.strip().lower()] = value.strip()
            websocket = headers.get("upgrade", "").lower() == "websocket"
            if method != "GET" or not path.startswith("/v1/realtime") or not websocket:
                log(cfg.log, {"event": "denied_path", "conn": conn_id, "host": host + " (direct)",
                              "method": method, "path": path[:200], "websocket": websocket})
                tls.sendall(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                return
            if "openai" not in cfg.relays:
                tls.sendall(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                return
            if not realtime_admit(cfg, conn_id, path):
                tls.sendall(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                return
            # Reuse the CONNECT path's forwarder (it relays the upgrade and frames both ways).
            Handler.forward(self, cfg, conn_id, host, method, path, head, headers, b"", tls, cfg.relays["openai"])
        except Exception as exc:
            log(cfg.log, {"event": "session_error", "conn": conn_id, "host": host + " (direct)",
                          "error": repr(exc)[:200]})
        finally:
            try:
                tls.close()
            except Exception:
                pass


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        cfg = self.server.cfg
        conn_id = "%d-%d" % (time.time_ns(), self.client_address[1])
        head = read_head(self.rfile)
        if not head:
            return
        parts = head[0].split()
        host = parts[1][:-4] if len(parts) == 3 and parts[1].endswith(":443") else None
        if parts[0:1] != ["CONNECT"] or (host not in REAL and host not in STANDIN):
            log(cfg.log, {"event": "denied_proxy_request", "conn": conn_id,
                          "request_line": head[0][:200]})
            self.wfile.write(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
        self.wfile.write(b"HTTP/1.1 200 Connection established\r\n\r\n")
        self.wfile.flush()
        try:
            tls = cfg.server_ctx.wrap_socket(self.connection, server_side=True)
        except Exception as exc:
            log(cfg.log, {"event": "client_tls_error", "conn": conn_id, "host": host,
                          "error": repr(exc)[:200]})
            return
        reader = tls.makefile("rb")
        try:
            head = read_head(reader)
            if not head:
                return
            method, path, _ = head[0].split(" ", 2)
            headers = {}
            for line in head[1:]:
                key, _, value = line.partition(":")
                headers[key.strip().lower()] = value.strip()
            body = read_body(reader, headers)
            if host in REAL:
                keyname, prefixes = REAL[host]
                if not path.startswith(prefixes) or method not in ("POST", "GET"):
                    log(cfg.log, {"event": "denied_path", "conn": conn_id, "host": host,
                                  "method": method, "path": path[:200]})
                    tls.sendall(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    return
                if host == "api.moonshot.ai" and method == "POST":
                    with LOCK:
                        SPEND["kimi_posts"] += 1
                        n = SPEND["kimi_posts"]
                    if n > cfg.moonshot_cap:
                        # Owner's spend limit for real Moonshot requests in this campaign.
                        log(cfg.log, {"event": "denied_spend_cap", "conn": conn_id, "host": host,
                                      "path": path[:200], "count": n, "cap": cfg.moonshot_cap})
                        msg = b'{"error":{"message":"test harness: Moonshot request limit reached"}}'
                        tls.sendall(b"HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n"
                                    b"Content-Length: %d\r\nConnection: close\r\n\r\n%s" % (len(msg), msg))
                        return
                if keyname not in cfg.relays:
                    log(cfg.log, {"event": "denied_no_key", "conn": conn_id, "host": host, "path": path[:200]})
                    tls.sendall(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    return
                if path.startswith("/v1/realtime"):
                    if headers.get("upgrade", "").lower() != "websocket" or not realtime_admit(cfg, conn_id, path):
                        tls.sendall(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        return
                self.forward(cfg, conn_id, host, method, path, head, headers, body, tls,
                             cfg.relays[keyname])
            else:
                self.forward(cfg, conn_id, host, method, path, head, headers, body, tls, None)
        except Exception as exc:
            log(cfg.log, {"event": "session_error", "conn": conn_id, "host": host,
                          "error": repr(exc)[:200]})
        finally:
            try:
                tls.close()
            except Exception:
                pass

    def forward(self, cfg, conn_id, host, method, path, head, headers, body, client, relay):
        started = time.time()
        websocket = headers.get("upgrade", "").lower() == "websocket"
        out = ["%s %s HTTP/1.1" % (method, path), "Host: " + host]
        for line in head[1:]:
            name = line.partition(":")[0].strip().lower()
            if name not in DROP or (relay is None and name == "authorization"):
                # Stand-ins get the client's own (synthetic) authorization; real hosts get none here: the
                # host-side relay adds the key (acct-rerun).
                out.append(line)
        if websocket:
            out += ["Upgrade: websocket", "Connection: Upgrade"]
        else:
            out += ["Content-Length: %d" % len(body), "Connection: close"]
        raw = ("\r\n".join(out) + "\r\n\r\n").encode("latin-1") + body
        if relay is not None:
            upstream = socket.create_connection(("127.0.0.1", relay), timeout=600)
        else:
            upstream = socket.create_connection(("127.0.0.1", STANDIN[host]), timeout=600)
        status = None
        total = 0
        tail = b""
        try:
            upstream.sendall(raw)
            del raw
            while True:
                chunk = upstream.recv(65536)
                if not chunk:
                    break
                if status is None:
                    first = chunk.split(b"\r\n", 1)[0].decode("latin-1")
                    status = first.split(" ")[1] if " " in first else first
                total += len(chunk)
                client.sendall(chunk)
                if websocket and status == "101":
                    # Relay frames both ways until either side closes.
                    upstream.settimeout(None)
                    back = threading.Thread(target=pipe, args=(client, upstream), daemon=True)
                    back.start()
                    tap = FrameTap(cfg, conn_id) if path.startswith("/v1/realtime") else None
                    rest = chunk.split(b"\r\n\r\n", 1)[1] if b"\r\n\r\n" in chunk else b""
                    if tap and rest:
                        tap.feed(rest)
                    pipe(upstream, client, tap)
                    back.join(5)
                    break
                tail = (tail + chunk)[-262144:]
        finally:
            upstream.close()
        row = {"event": "forwarded", "conn": conn_id, "host": host, "method": method,
               "path": path[:200], "status": status, "websocket": websocket,
               "response_bytes": total, "request_bytes": len(body),
               "real": relay is not None, "seconds": round(time.time() - started, 3)}
        row.update(summarize(body))
        if relay is not None and not websocket:
            row["usage"] = usage_from(tail)
        log(cfg.log, row)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--relay", action="append", default=[], help="name=port (zai, openai, kimi): host relay via tunnel")
    parser.add_argument("--cert", required=True)
    parser.add_argument("--cert-key", required=True)
    parser.add_argument("--upstream-ca", required=True)
    parser.add_argument("--log", required=True)
    parser.add_argument("--moonshot-cap", type=int, default=12)
    parser.add_argument("--realtime-cap", type=int, default=0)
    parser.add_argument("--realtime-port", type=int, default=0)
    parser.add_argument("--pin", action="append", default=[], help="HOST=IP for upstream connections")
    args = parser.parse_args()

    class Cfg:
        pass

    cfg = Cfg()
    cfg.relays = {}
    for item in args.relay:
        name, _, port = item.partition("=")
        cfg.relays[name] = int(port)
    cfg.pins = dict(item.split("=", 1) for item in args.pin)
    cfg.realtime_cap = args.realtime_cap
    cfg.log = args.log
    cfg.moonshot_cap = args.moonshot_cap
    cfg.server_ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    cfg.server_ctx.load_cert_chain(args.cert, args.cert_key)
    cfg.server_ctx.set_alpn_protocols(["http/1.1"])
    cfg.client_ctx = ssl.create_default_context(cafile=args.upstream_ca)
    cfg.client_ctx.set_alpn_protocols(["http/1.1"])
    server = Server(("127.0.0.1", args.port), Handler)
    server.cfg = cfg
    if args.realtime_port:
        # macOS lets an unprivileged account bind a port below 1024 only on the wildcard address. The guest's pf
        # drops every inbound packet on en0 except the host's SSH, and the handler refuses non-loopback peers.
        realtime = Server(("0.0.0.0", args.realtime_port), RealtimeHandler)
        realtime.cfg = cfg
        threading.Thread(target=realtime.serve_forever, daemon=True).start()
    log(cfg.log, {"event": "started", "port": args.port,
                  "real_hosts": {h: list(p) for h, (_, p) in REAL.items()},
                  "standin_hosts": STANDIN, "relays": cfg.relays,
                  "moonshot_cap": cfg.moonshot_cap, "realtime_cap": cfg.realtime_cap,
                  "realtime_port": args.realtime_port, "pins": cfg.pins})
    server.serve_forever()


if __name__ == "__main__":
    main()
