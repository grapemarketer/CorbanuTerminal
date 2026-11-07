"""Host-side gate in front of the owner's local model server (qwen-1052, 2026-10-20).

Listens on 127.0.0.1 only and forwards to the mlx-serve endpoint on 127.0.0.1. It never binds beyond
loopback; the Tart guest reaches it only through an SSH reverse tunnel. It:
  - allows only GET /v1/models and POST /v1/chat/completions (no admin or other endpoints);
  - sends one request at a time to the model server (the owner's rule), queueing the rest;
  - pauses new requests while macOS reports under 20% free memory (the brief's rule);
  - drops Authorization / API-key headers (the server needs none; the guest only sends a placeholder);
  - logs method, path, status, bytes, seconds, model, tool names and token usage. No bodies, no headers.
"""
import argparse
import json
import re
import socket
import socketserver
import subprocess
import threading
import time

ALLOWED = {("GET", "/v1/models"), ("POST", "/v1/chat/completions")}
DROP = {"authorization", "x-api-key", "proxy-authorization", "proxy-connection", "connection",
        "keep-alive", "transfer-encoding", "content-length", "host", "te", "upgrade"}
MODEL_LOCK = threading.Lock()
LOG_LOCK = threading.Lock()


def log(path, row):
    row["ts"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    row["time_ns"] = time.time_ns()
    with LOG_LOCK, open(path, "a") as out:
        out.write(json.dumps(row, sort_keys=True) + "\n")


def free_pct():
    try:
        out = subprocess.run(["/usr/bin/memory_pressure", "-Q"], capture_output=True, text=True,
                             timeout=20).stdout
        match = re.search(r"free percentage:\s*(\d+)%", out)
        return int(match.group(1)) if match else None
    except Exception:
        return None


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
    return {"model": data.get("model"), "tools": tools, "stream": data.get("stream"),
            "messages": len(data.get("messages") or [])}


def usage_from(tail):
    found = None
    for match in re.finditer(rb'"usage"\s*:\s*(\{[^{}]*\})', tail):
        try:
            found = json.loads(match.group(1))
        except Exception:
            pass
    return found


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
        if (method, path) not in ALLOWED:
            log(cfg.log, {"event": "denied", "conn": conn, "method": method, "path": path[:200]})
            self.wfile.write(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
        queued = time.time()
        with MODEL_LOCK:
            logged_pause = False
            while True:
                pct = free_pct()
                if pct is None or pct >= cfg.min_free:
                    break
                if not logged_pause:
                    log(cfg.log, {"event": "paused_memory", "conn": conn, "free_pct": pct})
                    logged_pause = True
                time.sleep(30)
            self.forward(cfg, conn, method, path, head, body, queued, pct)

    def forward(self, cfg, conn, method, path, head, body, queued, pct):
        started = time.time()
        out = ["%s %s HTTP/1.1" % (method, path), "Host: 127.0.0.1:%d" % cfg.upstream_port]
        for line in head[1:]:
            if line.partition(":")[0].strip().lower() not in DROP:
                out.append(line)
        out.append("Content-Length: %d" % len(body))
        out.append("Connection: close")
        raw = ("\r\n".join(out) + "\r\n\r\n").encode("latin-1") + body
        status, total, tail, error = None, 0, b"", None
        try:
            upstream = socket.create_connection(("127.0.0.1", cfg.upstream_port), timeout=1800)
            try:
                upstream.sendall(raw)
                while True:
                    chunk = upstream.recv(65536)
                    if not chunk:
                        break
                    if status is None:
                        first = chunk.split(b"\r\n", 1)[0].decode("latin-1")
                        status = first.split(" ")[1] if " " in first else first
                    total += len(chunk)
                    tail = (tail + chunk)[-16384:]
                    try:
                        self.wfile.write(chunk)
                        self.wfile.flush()
                    except OSError as exc:
                        error = "client closed: %r" % exc
                        break
            finally:
                upstream.close()
        except OSError as exc:
            error = repr(exc)[:200]
            try:
                self.wfile.write(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            except OSError:
                pass
        row = {"event": "forwarded", "conn": conn, "method": method, "path": path, "status": status,
               "request_bytes": len(body), "response_bytes": total,
               "queued_seconds": round(started - queued, 3), "seconds": round(time.time() - started, 3),
               "free_pct_at_start": pct, "usage": usage_from(tail)}
        if error:
            row["error"] = error
        row.update(summarize(body))
        log(cfg.log, row)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=11236)
    parser.add_argument("--upstream-port", type=int, default=11235)
    parser.add_argument("--min-free", type=int, default=20)
    parser.add_argument("--log", required=True)
    cfg = parser.parse_args()
    server = Server(("127.0.0.1", cfg.port), Handler)
    server.cfg = cfg
    log(cfg.log, {"event": "started", "listen": "127.0.0.1:%d" % cfg.port,
                  "upstream": "127.0.0.1:%d" % cfg.upstream_port,
                  "allowed": sorted(" ".join(a) for a in ALLOWED),
                  "min_free_pct": cfg.min_free, "free_pct": free_pct()})
    server.serve_forever()


if __name__ == "__main__":
    main()
