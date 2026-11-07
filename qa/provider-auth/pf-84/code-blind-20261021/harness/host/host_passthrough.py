"""Host-side blind pass-through for the live ChatGPT test account group only (pf84-cb1, 2026-10-21).

Listens on 127.0.0.1 only. Accepts `CONNECT host:443` for chatgpt.com and auth.openai.com, connects to the real host
and copies bytes both ways without opening TLS: the test account's tokens stay end-to-end encrypted between the guest
and OpenAI and never exist on the host in clear. Logs host, time and byte counts only.
usage: host_passthrough.py --port 11248 --log FILE
"""
import argparse
import json
import socket
import socketserver
import threading
import time

ALLOWED = {"chatgpt.com:443", "auth.openai.com:443"}
LOCK = threading.Lock()


def log(path, row):
    row["ts"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    with LOCK, open(path, "a") as out:
        out.write(json.dumps(row, sort_keys=True) + "\n")


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        cfg = self.server.cfg
        head = b""
        while b"\r\n\r\n" not in head and len(head) < 8192:
            chunk = self.connection.recv(1024)
            if not chunk:
                return
            head += chunk
        line = head.split(b"\r\n", 1)[0].decode("latin-1")
        parts = line.split()
        if len(parts) != 3 or parts[0] != "CONNECT" or parts[1] not in ALLOWED:
            log(cfg.log, {"event": "refused", "request_line": line[:200]})
            self.connection.sendall(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            return
        host, port = parts[1].rsplit(":", 1)
        started = time.time()
        try:
            up = socket.create_connection((host, int(port)), timeout=30)
        except Exception as exc:
            log(cfg.log, {"event": "connect_error", "target": parts[1], "error": repr(exc)[:200]})
            self.connection.sendall(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
            return
        up.settimeout(None)
        self.connection.sendall(b"HTTP/1.1 200 Connection established\r\n\r\n")
        counts = {"up": 0, "down": 0}

        def pump(src, dst, key):
            try:
                while True:
                    data = src.recv(65536)
                    if not data:
                        break
                    counts[key] += len(data)
                    dst.sendall(data)
            except Exception:
                pass
            finally:
                try:
                    dst.shutdown(socket.SHUT_WR)
                except Exception:
                    pass

        t = threading.Thread(target=pump, args=(self.connection, up, "up"), daemon=True)
        t.start()
        pump(up, self.connection, "down")
        t.join(timeout=5)
        up.close()
        log(cfg.log, {"event": "passthrough", "target": parts[1], "bytes_up": counts["up"],
                      "bytes_down": counts["down"], "seconds": round(time.time() - started, 3)})


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--log", required=True)
    args = parser.parse_args()

    class Cfg:
        pass

    cfg = Cfg()
    cfg.log = args.log
    server = Server(("127.0.0.1", args.port), Handler)
    server.cfg = cfg
    log(cfg.log, {"event": "started", "port": args.port, "allowed": sorted(ALLOWED)})
    server.serve_forever()


if __name__ == "__main__":
    main()
