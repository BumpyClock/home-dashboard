#!/usr/bin/env python3
"""Linux PTY check: host lock heartbeats continue while a real provider process blocks.

Uses synthetic empty calendar data, no accounts or hardware. Build frame-host-sync first.
"""
import importlib.util
import json
import os
import pty
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("serial_check", ROOT / "scripts/test-calendar-sync.py")
assert spec is not None and spec.loader is not None
serial_check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(serial_check)


def main():
    if sys.platform != "linux":
        print("SKIP watch PTY test: serialport's macOS baud ioctl rejects PTYs")
        return
    try:
        metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT))
    except (ValueError, subprocess.CalledProcessError) as error:
        raise AssertionError("could not resolve built host executable") from error
    host = Path(metadata["target_directory"]) / "debug/frame-host-sync"
    with tempfile.TemporaryDirectory(prefix="host-privacy-") as temporary:
        directory = Path(temporary)
        provider = directory / "provider"
        provider.write_text("#!/bin/sh\nset -eu\ntouch \"$0.running\"\nsleep 18\ntouch \"$0.done\"\nprintf '%s' '{\"complete\":true,\"events\":[]}'\n")
        provider.chmod(0o700)
        master, slave = pty.openpty()
        config = directory / "config.json"
        config.write_text(json.dumps({
            "connection": {"kind": "console", "port": os.ttyname(slave)},
            "calendar": {"sources": [{"kind": "workiq", "id": "fixture", "executable": str(provider)}]},
        }))
        heartbeats_during_fetch = []
        errors = []

        def console():
            try:
                for exchange in range(10):
                    assert serial_check.read_line(master).strip() == b""
                    if exchange == 0:
                        deadline = time.monotonic() + 3
                        while not directory.joinpath("provider.running").exists():
                            assert time.monotonic() < deadline, "provider acquisition waited for serial login"
                            time.sleep(0.01)
                    os.write(master, b"[user@frame ~]$ ")
                    assert b"FRAME_HOST_SHELL_V1" in serial_check.read_line(master)
                    os.write(master, b"\nFRAME_HOST_SHELL_V1\n")
                    command = serial_check.read_line(master)
                    if exchange == 0:
                        assert b"--host-state-receive" in command, "calendar delivery preceded privacy status"
                    if b"--host-state-receive" in command:
                        os.write(master, b"\nFRAME_HOST_STATE_READY_V1\n")
                        status = json.loads(serial_check.read_line(master))
                        assert status["version"] == 1
                        assert status["state"] in ("locked", "unlocked", "unknown")
                        if directory.joinpath("provider.running").exists() and not directory.joinpath("provider.done").exists():
                            heartbeats_during_fetch.append(status)
                        os.write(master, b"FRAME_HOST_STATE_STORED_V1\nFRAME_HOST_DONE_V1\n")
                    else:
                        assert b"--calendar-receive" in command
                        os.write(master, b"\nFRAME_CALENDAR_READY_V1\n")
                        snapshot = json.loads(serial_check.read_line(master))
                        assert snapshot["sources"][0]["events"] == []
                        os.write(master, b"FRAME_CALENDAR_STORED_V1\nFRAME_HOST_DONE_V1\n")
                        assert len(heartbeats_during_fetch) >= 2, "provider fetch blocked lock heartbeats"
                        return
                raise AssertionError("watch did not send completed provider result")
            except BaseException as error:
                errors.append(error)

        worker = threading.Thread(target=console, daemon=True)
        worker.start()
        with subprocess.Popen([str(host), "calendar", "watch", "--config", str(config)], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE) as process:
            try:
                worker.join(timeout=45)
                assert not worker.is_alive(), "watch protocol timed out"
                if errors:
                    raise errors[0]
                assert process.poll() is None, "watch unexpectedly exited"
                print(f"PASS {len(heartbeats_during_fetch)} lock heartbeats delivered during blocked provider fetch")
            finally:
                process.terminate()
                # Release any in-flight serial exchange before awaiting graceful shutdown.
                os.close(master)
                os.close(slave)
                process.wait(timeout=25)


if __name__ == "__main__":
    main()
