"""Disconnect the initial registry request, then proxy the next connection to sway."""

import select
import socket
import sys

with socket.socket(socket.AF_UNIX) as listener:
    listener.bind(sys.argv[1])
    listener.listen()
    first, _ = listener.accept()
    with first:
        first.settimeout(10)
        assert first.recv(4096), "no initial registry request"
    client, _ = listener.accept()
    with client, socket.socket(socket.AF_UNIX) as compositor:
        compositor.connect(sys.argv[2])
        peers = {client: compositor, compositor: client}
        while True:
            readable, _, _ = select.select(list(peers), [], [], 15)
            assert readable, "proxy timed out"
            for source in readable:
                data = source.recv(65536)
                if not data:
                    sys.exit(0)
                peers[source].sendall(data)
