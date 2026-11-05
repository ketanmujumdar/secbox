#!/usr/bin/env python3
"""Throwaway host-side listener for leak tests. Binds 127.0.0.1 only.

TCP: logs what arrives, and answers HTTP requests with a token.
UDP: logs what arrives, and answers with the token, so a probe that needs a UDP
     round trip (the QUIC probe) has something on the host that replies.
Usage: host-listener.py LOG_FILE PORT_FILE TOKEN
"""
import socket
import socketserver
import sys
import threading

LOG, PORT_FILE, TOKEN = sys.argv[1:4]
lock = threading.Lock()


def record(kind, peer, data):
    text = data[:512].decode("utf-8", "replace").replace("\n", " ").strip()
    with lock, open(LOG, "a") as f:
        f.write(f"{kind} {peer[0]} {text}\n")


class Tcp(socketserver.StreamRequestHandler):
    def handle(self):
        self.request.settimeout(3)
        try:
            data = self.request.recv(4096)
        except OSError:
            data = b""
        record("tcp", self.client_address, data)
        if data.startswith(b"GET "):
            body = (TOKEN + "\n").encode()
            self.request.sendall(
                b"HTTP/1.0 200 OK\r\nContent-Length: %d\r\n\r\n%s" % (len(body), body)
            )


class Udp(socketserver.BaseRequestHandler):
    max_packet_size = 8192

    def handle(self):
        data, sock = self.request
        record("udp", self.client_address, data)
        try:
            sock.sendto((TOKEN + "\n").encode(), self.client_address)
        except OSError:
            pass


class TcpServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


tcp = TcpServer(("127.0.0.1", 0), Tcp)
udp = socketserver.ThreadingUDPServer(("127.0.0.1", 0), Udp)
with open(PORT_FILE, "w") as f:
    f.write(f"{tcp.server_address[1]} {udp.server_address[1]}\n")
threading.Thread(target=udp.serve_forever, daemon=True).start()
tcp.serve_forever()
