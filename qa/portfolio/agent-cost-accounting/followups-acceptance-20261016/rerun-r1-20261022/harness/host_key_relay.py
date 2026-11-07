"""Host-side key relay for the acct-rerun Tart guest (2026-10-22). From host_zai_relay.py (qwen-1052 lineage).

The guest holds no real key. Its accounting mediator (acc_mediator.py, coordinator account) terminates the
product's TLS, then sends allowlisted requests as plain HTTP over one SSH reverse tunnel to this relay, which
listens on 127.0.0.1 only, adds the key and sends them to the real host over TLS. WebSocket upgrades (OpenAI
Responses over WebSocket) are relayed byte for byte after the 101.
One process per route: --route zai (api.z.ai) or --route openai (api.openai.com).
The key comes from the environment at start (RELAY_KEY, filled from the Corbanu vault by the command that starts
this process) and is removed from the environment at once.
Logs method/path/status/bytes/model only: no headers, no bodies, no key.
"""
import argparse
import json
import os
import socket
import socketserver
import ssl
import threading
import time

ROUTES = {
    "zai": ("api.z.ai", ("/api/paas/v4/", "/api/coding/paas/v4/", "/api/anthropic/")),
    "openai": ("api.openai.com", ("/v1/responses", "/v1/models")),
}
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


def model_of(body):
    try:
        return json.loads(body.decode("utf-8")).get("model")
    except Exception:
        return None


def pump(read, dst):
    """Copy until EOF; read is a callable returning bytes."""
    try:
        while True:
            data = read(65536)
            if not data:
                break
            dst.sendall(data)
    except OSError:
        pass
    finally:
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass


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
        websocket = headers.get("upgrade", "").lower() == "websocket"
        body = b"" if websocket else read_body(self.rfile, headers)
        if method not in ("GET", "POST") or not path.startswith(cfg.prefixes) or (websocket and method != "GET"):
            log(cfg.log, {"event": "denied_path", "conn": conn, "method": method, "path": path[:200],
                          "websocket": websocket})
            self.wfile.write(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
        started = time.time()
        out = ["%s %s HTTP/1.1" % (method, path), "Host: " + cfg.host]
        for line in head[1:]:
            if line.partition(":")[0].strip().lower() not in DROP:
                out.append(line)
        out.append("Authorization: Bearer " + cfg.key)
        if path.startswith("/api/anthropic/"):
            out.append("x-api-key: " + cfg.key)
        if websocket:
            out += ["Upgrade: websocket", "Connection: Upgrade"]
        else:
            out += ["Content-Length: %d" % len(body), "Connection: close"]
        raw = ("\r\n".join(out) + "\r\n\r\n").encode("latin-1") + body
        status, total, error = None, 0, None
        try:
            upstream = cfg.client_ctx.wrap_socket(socket.create_connection((cfg.host, 443), timeout=600),
                                                  server_hostname=cfg.host)
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
                    if websocket and status == "101":
                        self.wfile.flush()
                        upstream.settimeout(None)
                        self.connection.settimeout(None)
                        back = threading.Thread(target=pump, args=(self.rfile.read1, upstream), daemon=True)
                        back.start()
                        pump(upstream.recv, self.connection)
                        back.join(5)
                        break
            finally:
                upstream.close()
        except OSError as exc:
            error = repr(exc)[:200]
        row = {"event": "forwarded", "conn": conn, "method": method, "path": path[:200], "status": status,
               "websocket": websocket, "request_bytes": len(body), "first_response_bytes": total,
               "seconds": round(time.time() - started, 3), "model": model_of(body)}
        if error:
            row["error"] = error
        log(cfg.log, row)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--route", choices=sorted(ROUTES), required=True)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--log", required=True)
    cfg = parser.parse_args()
    cfg.key = os.environ.pop("RELAY_KEY", "").strip()
    if not cfg.key:
        raise SystemExit("RELAY_KEY is empty")
    cfg.host, cfg.prefixes = ROUTES[cfg.route]
    cfg.client_ctx = ssl.create_default_context(cafile="/etc/ssl/cert.pem")
    cfg.client_ctx.set_alpn_protocols(["http/1.1"])
    server = Server(("127.0.0.1", cfg.port), Handler)
    server.cfg = cfg
    log(cfg.log, {"event": "started", "route": cfg.route, "upstream": cfg.host,
                  "listen": "127.0.0.1:%d" % cfg.port, "allowed_prefixes": list(cfg.prefixes)})
    server.serve_forever()


if __name__ == "__main__":
    main()
