"""Check real terminal input and Ctrl-C using a pseudo-terminal (macOS/Linux)."""

import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import shlex
import struct
import sys
import tempfile
import termios
import time


BINARY = Path(__file__).resolve().parents[1] / "target/debug/spark"

with tempfile.TemporaryDirectory(prefix="spark-terminal-") as directory:
    pid, terminal = pty.fork()
    if pid == 0:
        os.chdir(directory)
        os.environ.pop("OPENAI_API_KEY", None)
        os.environ["TERM"] = "xterm-256color"
        os.execv(str(BINARY), [str(BINARY)])

    pending = b""

    def read_until(marker, count=1):
        global pending
        deadline = time.monotonic() + 5
        while pending.count(marker) < count:
            if time.monotonic() >= deadline:
                raise AssertionError(f"waiting for {marker!r}: {pending!r}")
            if select.select([terminal], [], [], 0.1)[0]:
                chunk = os.read(terminal, 8192)
                if not chunk:
                    raise AssertionError(f"terminal closed: {pending!r}")
                pending += chunk
        end = 0
        for _ in range(count):
            end = pending.index(marker, end) + len(marker)
        received, pending = pending[:end], pending[end:]
        return received

    reaped = False
    try:
        fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 120, 0, 0))
        read_until(b"> ")
        os.write(terminal, b"?\n")
        read_until(b"AI mode. Keep chatting normally; /shell returns to the shell.")
        read_until(b"ai> ")
        os.write(terminal, b"/clear\n")
        read_until(b"Conversation cleared.")
        read_until(b"ai> ")
        os.write(terminal, b"\x03")
        read_until(b"ai> ")
        os.write(terminal, b"/shell\n")
        read_until(b"Shell mode. Type ? to resume AI chat.")
        read_until(b"> ")
        child_code = "print('child-ready', flush=True); line = input(); print('child-echo:' + line, flush=True); input()"
        command = f"{shlex.quote(sys.executable)} -c {shlex.quote(child_code)}\n"
        os.write(terminal, command.encode())
        # Wait for real child output, not the line editor's echoed command.
        read_until(b"\r\nchild-ready\r\n")
        os.write(terminal, b"hello-from-stdin\n")
        read_until(b"\r\nchild-echo:hello-from-stdin\r\n")
        os.write(terminal, b"\x03")
        read_until(b"> ")
        os.write(terminal, b"pwd\n")
        read_until(b"\r\n" + str(Path(directory).resolve()).encode() + b"\r\n")
        read_until(b"> ")
        os.write(terminal, b"exit\n")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if select.select([terminal], [], [], 0)[0]:
                try:
                    pending += os.read(terminal, 8192)
                except OSError:
                    pass
            ended, status = os.waitpid(pid, os.WNOHANG)
            if ended:
                reaped = True
                assert os.waitstatus_to_exitcode(status) == 0
                break
            time.sleep(0.01)
        assert reaped, f"Spark did not exit: {pending!r}"
    finally:
        if not reaped:
            ended, _ = os.waitpid(pid, os.WNOHANG)
            if not ended:
                os.killpg(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
        os.close(terminal)

print("PASS: visible chat/shell prompts, mode retained on clear/Ctrl-C, terminal stdin, command interruption")
