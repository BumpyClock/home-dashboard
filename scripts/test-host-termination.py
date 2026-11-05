#!/usr/bin/env python3
"""Fixture-only POSIX termination checks. No serial devices or live providers."""

import argparse
import json
import os
import select
import signal
import socket
import subprocess
import tempfile
import time
from contextlib import suppress
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

PROVIDER = '''#!/usr/bin/env python3
import json
import os
import socket
import subprocess
import sys

role = "descendant" if sys.argv[1] == "--child" else "provider"
with socket.create_connection(ADDRESS, timeout=60) as connection:
    connection.sendall(json.dumps({"role": role, "group": os.getpgrp()}).encode() + b"\\n")
    if role == "provider":
        subprocess.Popen([sys.executable, __file__, "--child"],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    outcome = connection.recv(16)
    if role == "provider":
        if outcome != b"finish":
            raise SystemExit(7)
        print(json.dumps({"complete": True, "events": []}))
'''


def stopping(process):
    deadline = time.monotonic() + 5
    while True:
        remaining = deadline - time.monotonic()
        assert remaining > 0, "termination request produced no stopping feedback"
        assert select.select([process.stderr], [], [], remaining)[0], "no stopping feedback"
        line = process.stderr.readline()
        assert line, "CLI exited before settling its in-flight provider"
        if b"Stopping" in line:
            return


def check(host, mode, signum, provider_succeeds=True):
    with tempfile.TemporaryDirectory(prefix="host-termination-") as temporary, socket.socket() as listener:
        directory = Path(temporary)
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        listener.settimeout(10)
        provider = directory / "provider"
        provider.write_text(PROVIDER.replace("ADDRESS", repr(listener.getsockname())))
        provider.chmod(0o700)
        config = directory / "config.json"
        config.write_text(json.dumps({
            "connection": {"kind": "raw", "port": "nonexistent-termination-fixture-port"},
            "calendar": {"sources": [{"kind": "workiq", "id": "fixture", "executable": str(provider)}]},
        }))
        connections = {}
        groups = set()
        process = subprocess.Popen([str(host), "calendar", mode, "--config", str(config)],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True, bufsize=0)
        try:
            for _ in range(2):
                connection, _ = listener.accept()
                connection.settimeout(10)
                with connection.makefile("rb") as stream:
                    identity = json.loads(stream.readline())
                connections[identity["role"]] = connection
                groups.add(identity["group"])
            assert set(connections) == {"provider", "descendant"}
            assert process.pid not in groups, "fixture provider should have its own process group"
            if signum is not None:
                os.killpg(process.pid, signum)
                stopping(process)
                assert process.poll() is None, "CLI abandoned an active provider"
                os.killpg(process.pid, signum)
            connections["provider"].sendall(b"finish" if provider_succeeds else b"fail")
            stdout, stderr = process.communicate(timeout=10)
            assert process.returncode == 0, (process.returncode, stderr)
            if signum is None:
                assert json.loads(stdout)["sources"] == [{"id": "fixture", "events": []}]
            else:
                assert not stdout, "interrupted acquisition published calendar data"
                assert b"Calendar stored" not in stderr, "interrupted acquisition started a transfer"
            assert connections["descendant"].recv(1) == b"", "CLI exited before descendant cleanup"
            if signum is not None:
                print(f"PASS {mode} {signal.Signals(signum).name}, provider success={provider_succeeds}, no publication or orphan")
            else:
                print("PASS uninterrupted preview publishes and cleans up its descendants")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            for group in groups:
                with suppress(ProcessLookupError):
                    os.killpg(group, signal.SIGKILL)
            for connection in connections.values():
                connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", type=Path)
    args = parser.parse_args()
    if os.name != "posix":
        print("SKIP POSIX process-group signal checks")
        return
    if args.host:
        host = args.host.resolve()
    else:
        try:
            metadata = json.loads(subprocess.check_output(
                ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT))
        except (ValueError, subprocess.CalledProcessError) as error:
            raise AssertionError("could not resolve built host executable") from error
        host = Path(metadata["target_directory"]) / "debug/frame-host-sync"
    check(host, "preview", None)
    for mode in ("preview", "sync", "watch"):
        for signum in (signal.SIGINT, signal.SIGTERM):
            for provider_succeeds in (True, False):
                check(host, mode, signum, provider_succeeds)


if __name__ == "__main__":
    main()
