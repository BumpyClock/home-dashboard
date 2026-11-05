#!/usr/bin/env python3
"""Exercise serial wait and repository-fetch scheduling without hardware or network."""

import contextlib
import importlib.util
import io
import re
import sys
import threading
import types
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parent


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


with patch.dict(sys.modules, {"serial": types.ModuleType("serial")}):
    console = load("frame_console_fixture", "frame-console.py")
resolver = load("alarm_resolver_fixture", "resolve-alarm-packages.py")


class Clock:
    def __init__(self):
        self.now = 0

    def monotonic(self):
        return self.now

    def time_ns(self):
        return 123456789

    def sleep(self, duration):
        self.now += duration


class Serial:
    def __init__(self, clock, fresh=False, password=False, complete=True, status=0, decorated=False):
        self.clock = clock
        self.fresh = fresh
        self.password = password
        self.complete = complete
        self.status = status
        self.pending = bytearray()
        self.writes = []
        self.closed = False
        self.marker = None
        self.prompt = (b"\r\n\x1b[?2004l\r\x1b]3008;start=fixture;user=diag\x1b\\\x1b[?2004h[diag@frame ~]$ "
                       if decorated else b"[diag@frame ~]$ ")

    def write(self, data):
        self.writes.append(data)
        if data == b"\n":
            self.pending.extend(b"frame login:" if self.fresh else self.prompt)
        elif data == b"diag\n":
            self.pending.extend(b"Password:")
        elif data == b"fixture-password\n":
            if self.marker is None:
                self.pending.extend(self.prompt)
            else:
                self.finish()
        elif data.startswith(b"pkexec "):
            self.pending.extend(data)  # Echo must not count as command completion.
            marker = re.search(rb"ROOTEND[0-9]+", data)
            assert marker is not None
            self.marker = marker.group()
            if self.password:
                self.pending.extend(b"Password:")
            else:
                self.finish()
        return len(data)

    def finish(self):
        if self.complete:
            assert self.marker is not None
            self.pending.extend(b"result\r\n" + self.marker + str(self.status).encode() + b"\r\n")

    def read(self, _size):
        self.clock.sleep(0.01)
        chunk = bytes(self.pending[:3])  # Fragment every prompt and completion line.
        del self.pending[:3]
        return chunk

    def flush(self):
        pass

    def reset_input_buffer(self):
        self.pending.clear()

    def close(self):
        self.closed = True


class ConsoleChecks(unittest.TestCase):
    def root(self, **settings):
        clock = Clock()
        serial = Serial(clock, **settings)
        output = io.StringIO()
        credentials = {"username": "diag", "password": "fixture-password"}
        with patch.object(console, "time", clock), patch.object(console, "load_creds", return_value=credentials), \
                patch.object(console, "open_port", return_value=serial), contextlib.redirect_stdout(output):
            if settings.get("complete", True):
                console.cmd_root(types.SimpleNamespace(command="printf 'hello'"))
            else:
                with self.assertRaises(TimeoutError):
                    console.cmd_root(types.SimpleNamespace(command="printf 'hello'"))
        self.assertTrue(serial.closed)
        self.assertEqual(sum(data.startswith(b"pkexec ") for data in serial.writes), 1)
        self.assertFalse(any(data.startswith(b"echo ROOTEND") for data in serial.writes))
        return clock, serial, output.getvalue()

    def test_cached_auth_and_existing_shell_do_not_exhaust_waits(self):
        clock, serial, output = self.root(status=7)
        self.assertLess(clock.now, 3)
        self.assertNotIn(b"fixture-password\n", serial.writes)
        self.assertIn("ROOTEND1234567897\r\n", output)

    def test_fragmented_login_and_password_prompts_still_authenticate(self):
        clock, serial, _ = self.root(fresh=True, password=True)
        self.assertLess(clock.now, 3)
        self.assertEqual(serial.writes.count(b"diag\n"), 1)
        self.assertEqual(serial.writes.count(b"fixture-password\n"), 2)

    def test_carriage_return_and_osc_prompt_reaches_existing_or_fresh_shell(self):
        for fresh in (False, True):
            with self.subTest(fresh=fresh):
                clock, _, _ = self.root(fresh=fresh, decorated=True)
                self.assertLess(clock.now, 3)

    def test_echoed_marker_is_not_completion_and_timeout_closes_port(self):
        clock, _, _ = self.root(complete=False)
        self.assertGreaterEqual(clock.now, 60)
        self.assertLess(clock.now, 61)

    def test_unknown_console_state_fails_without_sending_commands(self):
        clock = Clock()
        serial = Serial(clock)
        serial.read = lambda _size: (clock.sleep(0.2) or b"unrecognized output\n")
        with patch.object(console, "time", clock), self.assertRaises(TimeoutError):
            console.login(serial, {"username": "diag", "password": "fixture-password"})
        self.assertEqual(serial.writes, [b"\n"])


class RepositoryChecks(unittest.TestCase):
    def resolve(self, fail=None):
        gate = threading.Barrier(2, timeout=3)
        extra_done = threading.Event()
        observed = []

        def entries(_mirror, repo):
            observed.append(repo)
            gate.wait()
            if repo == "core":
                self.assertTrue(extra_done.wait(3))
            else:
                extra_done.set()
            if repo == fail:
                raise OSError("fixture repository unavailable")
            yield {"NAME": "shared", "VERSION": "2", "FILENAME": repo + ".pkg", "SHA256SUM": repo}

        output = io.StringIO()
        errors = io.StringIO()
        with patch.object(resolver, "entries", entries), patch.object(sys, "argv", ["resolver", "https://fixture.invalid"]), \
                patch.object(sys, "stdin", io.StringIO("shared 1\n")), contextlib.redirect_stdout(output), \
                contextlib.redirect_stderr(errors):
            if fail:
                with self.assertRaises(OSError):
                    resolver.main()
            else:
                resolver.main()
        self.assertCountEqual(observed, ["core", "extra"])
        return output.getvalue(), errors.getvalue()

    def test_both_databases_start_and_core_precedence_survives_reverse_completion(self):
        output, errors = self.resolve()
        self.assertEqual(output, "core\tcore.pkg\tcore\n")
        self.assertIn("pinned shared 1, mirror has 2", errors)

    def test_either_failed_database_prevents_resolution_output(self):
        for repo in ("core", "extra"):
            with self.subTest(repo=repo):
                output, _ = self.resolve(fail=repo)
                self.assertEqual(output, "")


if __name__ == "__main__":
    unittest.main()
