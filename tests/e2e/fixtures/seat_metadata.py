"""Send registry globals, then sync.done and wl_seat.name in the same batch.

The daemon binds the seat while waiting for sync, before its session exists.
No input is activated: the fixture only serves the registry and seat metadata.
"""

import socket
import struct
import sys


def uint(value):
    return struct.pack("=I", value)


def string(value):
    data = value.encode() + b"\0"
    return uint(len(data)) + data + b"\0" * (-len(data) % 4)


def event(object_id, opcode, payload):
    return struct.pack("=II", object_id, ((len(payload) + 8) << 16) | opcode) + payload


with socket.socket(socket.AF_UNIX) as listener:
    listener.bind(sys.argv[1])
    listener.listen()
    connection, _ = listener.accept()
    with connection:
        connection.settimeout(15)
        pending = b""
        registry = sync = seat = None
        sent = False
        while chunk := connection.recv(4096):
            pending += chunk
            while len(pending) >= 8:
                object_id, header = struct.unpack("=II", pending[:8])
                size, opcode = header >> 16, header & 65535
                assert size >= 8
                if len(pending) < size:
                    break
                body, pending = pending[8:size], pending[size:]
                if object_id == 1 and opcode == 1:  # wl_display.get_registry
                    registry = struct.unpack("=I", body)[0]
                    connection.sendall(b"".join(
                        event(registry, 0, uint(name) + string(interface) + uint(version))
                        for name, interface, version in [
                            (1, "zwp_input_method_manager_v2", 1),
                            (2, "zwp_virtual_keyboard_manager_v1", 1),
                            (3, "wl_seat", 7),
                        ]
                    ))
                elif object_id == 1 and opcode == 0:  # wl_display.sync
                    sync = struct.unpack("=I", body)[0]
                elif object_id == registry and opcode == 0:  # wl_registry.bind
                    if struct.unpack("=I", body[:4])[0] == 3:
                        seat = struct.unpack("=I", body[-4:])[0]
                if sync is not None and seat is not None and not sent:
                    connection.sendall(
                        event(sync, 0, uint(1))
                        + event(1, 1, uint(sync))
                        + event(seat, 0, uint(0))
                        + event(seat, 1, string("seat0"))
                    )
                    sent = True
