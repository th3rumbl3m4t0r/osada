#!/usr/bin/python3 -I
"""A stand-in for Stalwart's https listener behind an edge's passthrough
(D-U16): every connection must start with a PROXY protocol v2 header, then
TLS. It answers any request with `STALWART-OK from <address>:<port>`, the
visitor's address as the header told it.

    python3 -I tests/fake_stalwart.py <port> <fullchain.pem> <privkey.pem>
"""
import socket, ssl, struct, sys, threading

SIG = b"\r\n\r\n\x00\r\nQUIT\n"


def recvn(c, n):
    b = b""
    while len(b) < n:
        x = c.recv(n - len(b))
        if not x:
            raise EOFError
        b += x
    return b


def handle(ctx, c):
    try:
        h = recvn(c, 16)
        if h[:12] != SIG or h[12] != 0x21:
            c.sendall(b"no PROXY v2 header\n")
            print("refused: no PROXY header", flush=True)
            return
        body = recvn(c, struct.unpack("!H", h[14:16])[0])
        if h[13] == 0x11:
            src, port = socket.inet_ntop(socket.AF_INET, body[0:4]), struct.unpack("!H", body[8:10])[0]
        else:
            src, port = socket.inet_ntop(socket.AF_INET6, body[0:16]), struct.unpack("!H", body[32:34])[0]
        t = ctx.wrap_socket(c, server_side=True)
        t.recv(65536)
        msg = f"STALWART-OK from {src}:{port}\n".encode()
        t.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\nContent-Length: %d\r\n\r\n" % len(msg) + msg)
        print("answered", src, port, flush=True)
        t.close()
    except Exception as e:
        print("error:", e, flush=True)
    finally:
        c.close()


def main():
    port, cert, key = int(sys.argv[1]), sys.argv[2], sys.argv[3]
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(cert, key)
    ls = socket.socket()
    ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    ls.bind(("127.0.0.1", port))
    ls.listen(16)
    while True:
        c, _ = ls.accept()
        threading.Thread(target=handle, args=(ctx, c), daemon=True).start()


if __name__ == "__main__":
    main()
