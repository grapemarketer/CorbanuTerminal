"""Host-side Z.AI relay for the Tart guest's tester (qwen-1052, 2026-10-20).

The guest holds no real key: its mediator sends allowlisted Z.AI inference requests as plain HTTP over an
SSH reverse tunnel to this relay, which listens on 127.0.0.1 only, adds the key and sends them to
api.z.ai over TLS. The key comes from the environment at start (ZAI_RELAY_KEY, filled from the Corbanu vault
by the command that starts this process) and is removed from the environment at once.
Logs method/path/status/bytes/model/tool names only: no headers, no bodies, no key.
"""
import argparse
import json
import os
import socket
import socketserver
import ssl
import threading
import time

UPSTREAM = ("api.z.ai", 443)
ALLOWED_PREFIXES = ("/api/paas/v4/", "/api/coding/paas/v4/", "/api/anthropic/")
DROP = {"authorization", "x-api-key", "proxy-authorization", "proxy-connection", "connection",
        "keep-alive", "transfer-encoding", "content-length", "host", "te", "upgrade"}
LOG_LOCK = threading.Lock()


def log(path, row):
    row["ts"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    row["time_ns"] = time.time_ns()
    with LOG_LOCK, open(path, "a") as out:
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
    tools = []
    for tool in data.get("tools") or []:
        if isinstance(tool, dict):
            fn = tool.get("function") if isinstance(tool.get("function"), dict) else {}
            tools.append(fn.get("name") or tool.get("name") or tool.get("type"))
    role = "executor" if b"independent functional executor" in body else "product"
    return {"model": data.get("model"), "tools": tools, "stream": data.get("stream"), "role": role}


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        cfg = self.server.cfg
        conn = "%d-%d" % (time.time_ns(), self.client_address[1])
        head = read_head(self.rfile)
        if not head:
            return
        try:
            method, path, _ = head[0].split(" ", 2)
        except ValueError:
            return
        headers = {}
        for line in head[1:]:
            key, _, value = line.partition(":")
            headers[key.strip().lower()] = value.strip()
        body = read_body(self.rfile, headers)
        if method not in ("GET", "POST") or not path.startswith(ALLOWED_PREFIXES):
            log(cfg.log, {"event": "denied_path", "conn": conn, "method": method, "path": path[:200]})
            self.wfile.write(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
        started = time.time()
        out = ["%s %s HTTP/1.1" % (method, path), "Host: api.z.ai"]
        for line in head[1:]:
            if line.partition(":")[0].strip().lower() not in DROP:
                out.append(line)
        out.append("Authorization: Bearer " + cfg.key)
        if path.startswith("/api/anthropic/"):
            out.append("x-api-key: " + cfg.key)
        out.append("Content-Length: %d" % len(body))
        out.append("Connection: close")
        raw = ("\r\n".join(out) + "\r\n\r\n").encode("latin-1") + body
        status, total, error = None, 0, None
        try:
            upstream = cfg.client_ctx.wrap_socket(socket.create_connection(UPSTREAM, timeout=600),
                                                  server_hostname=UPSTREAM[0])
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
                    self.wfile.write(chunk)
            finally:
                upstream.close()
        except OSError as exc:
            error = repr(exc)[:200]
        row = {"event": "forwarded", "conn": conn, "method": method, "path": path, "status": status,
               "request_bytes": len(body), "response_bytes": total, "seconds": round(time.time() - started, 3)}
        if error:
            row["error"] = error
        row.update(summarize(body))
        log(cfg.log, row)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=11237)
    parser.add_argument("--log", required=True)
    cfg = parser.parse_args()
    cfg.key = os.environ.pop("ZAI_RELAY_KEY", "").strip()
    if not cfg.key:
        raise SystemExit("ZAI_RELAY_KEY is empty")
    cfg.client_ctx = ssl.create_default_context(cafile="/etc/ssl/cert.pem")
    cfg.client_ctx.set_alpn_protocols(["http/1.1"])
    server = Server(("127.0.0.1", cfg.port), Handler)
    server.cfg = cfg
    log(cfg.log, {"event": "started", "listen": "127.0.0.1:%d" % cfg.port, "allowed_prefixes": list(ALLOWED_PREFIXES)})
    server.serve_forever()


if __name__ == "__main__":
    main()
