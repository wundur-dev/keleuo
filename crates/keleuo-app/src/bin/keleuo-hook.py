#!/usr/bin/env python3
# keleuo-hook.py <socket> <token> <event>: keleuo-hook (keleuo-hook.rs) for an agent on another
# machine, which has no Keleuo binary. Keleuo writes it there for each session it starts over
# ssh; the socket there is forwarded by that connection to the window's. The same line goes
# out, a permission request waits for the answer the same way, and when the window is not
# there it prints nothing, so the agent asks in its own interface.

import socket
import sys


def clean(s):
    return "".join(c for c in s if c.isascii() and (c.isalnum() or c in "-_"))


def main():
    if len(sys.argv) < 4:
        return
    path, token, event = sys.argv[1:4]
    payload = sys.stdin.read().strip().replace("\n", " ").replace("\r", " ") or "null"
    line = '{"token":"%s","event":"%s","payload":%s}\n' % (clean(token), clean(event), payload)
    try:
        conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        conn.connect(path)
        conn.sendall(line.encode())
        if event != "PermissionRequest":
            return
        answer = conn.makefile(encoding="utf-8").readline().strip()
    except OSError:
        return
    if answer:
        sys.stdout.write(answer)
        sys.stdout.flush()


main()
