"""Check real terminal input and Ctrl-C using a pseudo-terminal (macOS/Linux)."""

import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import tempfile
import termios
import time


BINARY = Path(__file__).resolve().parents[1] / "target/debug/spark"

with tempfile.TemporaryDirectory(prefix="spark-terminal-") as directory:
    pid, terminal = pty.fork()
    if pid == 0:
        os.chdir(directory)
        os.environ.pop("OPENAI_API_KEY", None)
        os.execv(str(BINARY), [str(BINARY)])

    def read_until(marker, count=1):
        received = b""
        deadline = time.monotonic() + 5
        while received.count(marker) < count:
            if time.monotonic() >= deadline:
                raise AssertionError(f"waiting for {marker!r}: {received!r}")
            if select.select([terminal], [], [], 0.1)[0]:
                chunk = os.read(terminal, 8192)
                if not chunk:
                    raise AssertionError(f"terminal closed: {received!r}")
                received += chunk
        return received

    reaped = False
    try:
        fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 120, 0, 0))
        read_until(b"> ")
        os.write(terminal, b"cat\n")
        # Both terminal echo and cat's output must appear before interrupting.
        os.write(terminal, b"hello-from-stdin\n")
        read_until(b"hello-from-stdin", count=2)
        os.write(terminal, b"\x03")
        read_until(b"> ")
        os.write(terminal, b"pwd\n")
        output = read_until(b"> ")
        assert str(Path(directory).resolve()).encode() in output, output
        os.write(terminal, b"exit\n")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            ended, status = os.waitpid(pid, os.WNOHANG)
            if ended:
                reaped = True
                assert os.waitstatus_to_exitcode(status) == 0
                break
            time.sleep(0.01)
        assert reaped, "Spark did not exit"
    finally:
        if not reaped:
            os.killpg(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        os.close(terminal)

print("PASS: terminal stdin reaches programs; Ctrl-C returns to Spark; shell continues")
