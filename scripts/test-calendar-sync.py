#!/usr/bin/env python3
"""Exercise real Rust host -> pseudo-serial console -> real frame receiver on Linux.

macOS uses subprocess pipes because serialport's macOS baud ioctl rejects PTYs.

Uses only Python's standard library. No hardware, accounts, or work-calendar data.
Run after `cargo build -p frame-host-sync -p photoframe_proto`.
"""
import json
import os
import pty
import select
import shlex
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def read_line(fd, timeout=15):
    deadline = time.monotonic() + timeout
    line = bytearray()
    while time.monotonic() < deadline:
        if select.select([fd], [], [], 0.2)[0]:
            byte = os.read(fd, 1)
            if not byte:
                raise AssertionError("pseudo-serial stream closed")
            line.extend(byte)
            if byte == b"\n":
                return bytes(line)
    raise AssertionError("pseudo-serial line timed out")


def run():
    try:
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT))
    except (subprocess.CalledProcessError, ValueError) as error:
        raise AssertionError("could not read Cargo target directory") from error
    binary_dir = Path(metadata["target_directory"]) / "debug"
    host = binary_dir / "frame-host-sync"
    frame = binary_dir / "photoframe_proto"
    with tempfile.TemporaryDirectory(prefix="frame-calendar-") as temp:
        directory = Path(temp)
        source = directory / "events.json"
        target = directory / "stored.json"
        # Bigger than a canonical TTY line; includes Unicode and shell-looking event text.
        events = [{"id": f"work-{i}", "uid": f"uid-{i}",
                   "title": f"Café review {i} $(touch should-not-exist)", "detail": "Room 4",
                   "time": {"kind": "timed", "start": "2026-11-02T18:00:00Z",
                            "end": "2026-11-02T19:00:00Z"}} for i in range(80)]
        source.write_text(json.dumps(events))
        config = directory / "host.json"
        for expected in [events, []]:
            source.write_text(json.dumps(expected))
            if sys.platform != "linux":
                config.write_text(json.dumps({"calendar": {"sources": [
                    {"kind": "json", "id": "work", "path": "events.json"}]}}))
                wire = subprocess.check_output([str(host), "calendar", "preview", "--config", str(config)])
                result = subprocess.run([str(frame), "--calendar-receive"], input=wire,
                                        env={**os.environ, "FRAME_CALENDAR_HOST": str(target)},
                                        capture_output=True, timeout=10)
                assert result.returncode == 0, result.stderr
                assert b"FRAME_CALENDAR_STORED_V1" in result.stdout
                try:
                    assert json.loads(target.read_text())["sources"][0]["events"] == expected
                except (OSError, ValueError) as error:
                    raise AssertionError("could not read persisted snapshot") from error
                print(f"PASS subprocess pipes -> frame persisted {len(expected)} events (no serial PTY)")
                continue
            master, slave = pty.openpty()
            config.write_text(json.dumps({
                "connection": {"kind": "console", "port": os.ttyname(slave),
                               "username": "test-user", "password_env": "FRAME_TEST_PASSWORD"},
                "calendar": {"sources": [{"kind": "json", "id": "work", "path": "events.json"}]}}))
            errors = []

            def console(master=master, expected=expected, errors=errors):
                try:
                    assert read_line(master).strip() == b""
                    os.write(master, b"frame login: ")
                    assert read_line(master).strip() == b"test-user"
                    os.write(master, b"Password: ")
                    assert read_line(master).strip() == b"test-password"
                    os.write(master, b"\r\n[test-user@frame ~]$ ")
                    assert b"printf" in read_line(master)
                    os.write(master, b"\r\nFRAME_HOST_SHELL_V1\r\n")
                    command = read_line(master)
                    # The frame's interactive shell emits OSC metadata without a trailing newline.
                    os.write(master, b"\x1b]3008;start=fixture\x1b\\")
                    command = command.decode().replace(
                        "/opt/photoframe-proto/current/photoframe_proto", shlex.quote(str(frame)))
                    env = {**os.environ, "FRAME_CALENDAR_HOST": str(target)}
                    with subprocess.Popen(["sh", "-c", command], env=env,
                                          stdin=master, stdout=master, stderr=subprocess.PIPE) as receiver:
                        assert receiver.stderr is not None
                        assert receiver.wait(timeout=35) == 0, receiver.stderr.read().decode()
                        assert json.loads(target.read_text())["sources"][0]["events"] == expected
                except BaseException as error:
                    errors.append(error)

            worker = threading.Thread(target=console, daemon=True)
            worker.start()
            try:
                result = subprocess.run([str(host), "calendar", "sync", "--config", str(config)],
                                        env={**os.environ, "FRAME_TEST_PASSWORD": "test-password"},
                                        capture_output=True, text=True, timeout=50)
                worker.join(timeout=5)
                assert not worker.is_alive(), f"console worker did not finish. host rc={result.returncode}, stderr={result.stderr}"
                if errors:
                    raise errors[0]
                assert result.returncode == 0, result.stderr
                assert not (ROOT / "should-not-exist").exists()
                print(f"PASS serial console -> frame persisted {len(expected)} events")
            finally:
                os.close(master)
                os.close(slave)
        # Invalid source must fail before opening or writing to a serial port.
        source.write_text("not JSON")
        result = subprocess.run([str(host), "calendar", "preview", "--config", str(config)],
                                capture_output=True, text=True, timeout=10)
        assert result.returncode != 0
        assert "calendar source work failed" in result.stderr
        try:
            assert json.loads(target.read_text())["sources"][0]["events"] == []
        except (OSError, ValueError) as error:
            raise AssertionError("could not read final frame snapshot") from error
        print("PASS provider failure leaves stored snapshot untouched")


if __name__ == "__main__":
    run()
