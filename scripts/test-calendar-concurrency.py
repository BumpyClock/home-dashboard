#!/usr/bin/env python3
"""Fixture-only checks and matched latency probe for calendar source acquisition."""

import argparse
import json
import os
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def parse_json(data):
    try:
        return json.loads(data)
    except ValueError as error:
        raise AssertionError("fixture or host returned invalid JSON") from error


def host_binary():
    metadata = parse_json(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT,
    ))
    return Path(metadata["target_directory"]) / "debug/frame-host-sync"


def configuration(directory, sources):
    path = directory / "config.json"
    path.write_text(json.dumps({"calendar": {"time_zone": "UTC", "sources": sources}}))
    return path


def command(host, config):
    return [str(host), "calendar", "preview", "--config", str(config)]


class Providers:
    def __init__(self, directory, count, listener=None, delay=0):
        self.sources = []
        for index in range(count):
            identity = f"source{index}"
            provider = directory / identity
            if listener is None:
                work = f"time.sleep({delay!r})"
            else:
                work = f"""with socket.create_connection({listener.getsockname()!r}, timeout=5) as connection:
    connection.sendall(json.dumps({{"id": {identity!r}, "question": sys.argv[3]}}).encode() + b"\\n")
    outcome = connection.recv(16)
if outcome == b"fail":
    raise SystemExit(7)
if outcome != b"release":
    raise SystemExit(8)"""
            provider.write_text(f"""#!{sys.executable}
import json
import socket
import sys
import time
{work}
print(json.dumps({{"complete": True, "events": [{{
    "id": "occurrence", "uid": "shared", "title": {identity!r}, "detail": "",
    "time": {{"kind": "timed", "start": "2026-11-03T18:00:00Z", "end": "2026-11-03T19:00:00Z"}}
}}]}}))
""")
            provider.chmod(0o700)
            self.sources.append({"kind": "workiq", "id": identity, "executable": str(provider)})


def accept(listener, connections):
    connection, _ = listener.accept()
    connection.settimeout(5)
    connections.append(connection)
    with connection.makefile("rb") as stream:
        request = parse_json(stream.readline())
    return request["id"], (connection, request["question"])


def assert_no_start(listener):
    listener.settimeout(0.2)
    try:
        unexpected, _ = listener.accept()
    except TimeoutError:
        return
    else:
        unexpected.close()
        raise AssertionError("more than two providers started before an active provider settled")
    finally:
        listener.settimeout(5)


def overlap_check(host, fail=False):
    with tempfile.TemporaryDirectory(prefix="calendar-overlap-") as temporary, socket.socket() as listener:
        directory = Path(temporary)
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        listener.settimeout(5)
        providers = Providers(directory, 4, listener=listener)
        config = configuration(directory, providers.sources)
        connections = []
        with subprocess.Popen(command(host, config), stdout=subprocess.PIPE, stderr=subprocess.PIPE) as process:
            try:
                active = dict(accept(listener, connections) for _ in range(2))
                assert set(active) == {"source0", "source1"}, active
                questions = [request for _, request in active.values()]
                assert_no_start(listener)
                active["source1"][0].sendall(b"fail" if fail else b"release")
                if fail:
                    # Keep the earlier source in flight while the later failure settles.
                    assert_no_start(listener)
                    active["source0"][0].sendall(b"fail")
                else:
                    for expected in ("source2", "source3"):
                        identity, (connection, question) = accept(listener, connections)
                        assert identity == expected, identity
                        questions.append(question)
                        connection.sendall(b"release")
                    active["source0"][0].sendall(b"release")
                stdout, stderr = process.communicate(timeout=10)
                if fail:
                    assert process.returncode != 0
                    assert not stdout, "failed acquisition published a partial snapshot"
                    assert b"calendar source source0 failed" in stderr, stderr
                    assert_no_start(listener)
                else:
                    assert process.returncode == 0, stderr.decode()
                    snapshot = parse_json(stdout)
                    assert [source["id"] for source in snapshot["sources"]] == [
                        "source0", "source1", "source2", "source3",
                    ], snapshot
                    assert [source["events"][0]["title"] for source in snapshot["sources"]] == [
                        "source0", "source1", "source2", "source3",
                    ], snapshot
                    assert len(set(questions)) == 1, "sources used different query windows"
            finally:
                for connection in connections:
                    connection.close()
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)


def json_checks(host):
    with tempfile.TemporaryDirectory(prefix="calendar-json-") as temporary:
        directory = Path(temporary)
        (directory / "events.json").write_text("[]")
        source = {"kind": "json", "id": "json", "path": "events.json"}
        providers = Providers(directory, 1)
        for sources in ([], [source], [source, *providers.sources]):
            result = subprocess.run(command(host, configuration(directory, sources)),
                                    capture_output=True, check=True, timeout=10)
            assert [row["id"] for row in parse_json(result.stdout)["sources"]] == [
                row["id"] for row in sources
            ]
        (directory / "events.json").write_text("invalid JSON")
        result = subprocess.run(command(host, configuration(directory, [source, *providers.sources])),
                                capture_output=True, timeout=10)
        assert result.returncode != 0 and not result.stdout, result


def benchmark(host, samples, delay):
    with tempfile.TemporaryDirectory(prefix="calendar-latency-") as temporary:
        directory = Path(temporary)
        config = configuration(directory, Providers(directory, 4, delay=delay).sources)
        durations = []
        for _ in range(samples):
            started = time.perf_counter()
            result = subprocess.run(command(host, config), capture_output=True, check=True, timeout=15)
            durations.append(time.perf_counter() - started)
            assert len(parse_json(result.stdout)["sources"]) == 4
        print(json.dumps({"sources": 4, "provider_delay_seconds": delay,
                          "samples_seconds": durations, "median_seconds": statistics.median(durations)}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", type=Path)
    parser.add_argument("--benchmark", action="store_true")
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--delay", type=float, default=0.2)
    args = parser.parse_args()
    if os.name != "posix":
        print("SKIP executable fixture scripts require POSIX")
        return
    host = (args.host or host_binary()).resolve()
    if args.benchmark:
        benchmark(host, args.samples, args.delay)
    else:
        overlap_check(host)
        print("PASS bounded, work-conserving provider overlap with ordered complete snapshots")
        overlap_check(host, fail=True)
        print("PASS deterministic source errors, settled workers, no partial snapshot or further dispatch")
        json_checks(host)
        print("PASS empty, single, mixed JSON/process sources and invalid JSON retention")


if __name__ == "__main__":
    main()
