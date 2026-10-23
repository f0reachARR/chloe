"""公開したポートの中継を確かめるための、VM の中の小さなサーバー。

    python3 proxy-server.py PORT [CERT KEY]

127.0.0.1 だけで待ち受ける (VM の外から直には届かない)。CERT と KEY を渡すと HTTPS で話す。

    GET /headers   受けたヘッダーを JSON で返す (chloe の cookie とトークンが届かないことを確かめる)
    GET /ws        WebSocket に切り替え、受けたテキストに "echo:" を付けて返す。Vite の HMR と同じく、
                   Origin があって localhost のものでなければ 403 で断る
    GET /*         "hello from PORT" と、アプリの cookie を一つ渡す
"""

import base64
import hashlib
import http.server
import json
import ssl
import struct
import sys

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        if self.path == "/headers":
            self.reply(200, json.dumps({k.lower(): v for k, v in self.headers.items()}))
        elif self.path == "/ws" and self.headers.get("Upgrade", "").lower() == "websocket":
            self.websocket()
        else:
            self.reply(200, f"hello from {self.server.server_port}", cookie="app=1; Path=/")

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.reply(200, f"posted {body[:40].decode()} ({len(body)} bytes)")

    def reply(self, status, body, cookie=None):
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(data)))
        if cookie:
            self.send_header("Set-Cookie", cookie)
        self.end_headers()
        self.wfile.write(data)

    def websocket(self):
        origin = self.headers.get("Origin")
        port = self.server.server_port
        if origin and origin not in (f"http://localhost:{port}", f"https://localhost:{port}"):
            self.reply(403, f"origin {origin} is not allowed")
            return
        key = self.headers["Sec-WebSocket-Key"]
        accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        self.wfile.flush()
        while True:
            head = self.rfile.read(2)
            if len(head) < 2:
                return
            opcode, length = head[0] & 0x0F, head[1] & 0x7F
            if length == 126:
                (length,) = struct.unpack(">H", self.rfile.read(2))
            elif length == 127:
                (length,) = struct.unpack(">Q", self.rfile.read(8))
            mask = self.rfile.read(4) if head[1] & 0x80 else b"\0\0\0\0"
            payload = bytes(b ^ mask[i % 4] for i, b in enumerate(self.rfile.read(length)))
            if opcode == 8:
                return
            if opcode == 1:
                out = b"echo:" + payload
                self.wfile.write(bytes([0x81, len(out)]) + out)
                self.wfile.flush()

    def log_message(self, format, *args):
        pass


def main():
    port = int(sys.argv[1])
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    if len(sys.argv) == 4:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(sys.argv[2], sys.argv[3])
        server.socket = context.wrap_socket(server.socket, server_side=True)
    print(f"listening on {port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
