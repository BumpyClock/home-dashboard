#!/usr/bin/env python3
"""Drive the photoframe over its USB serial console.

Subcommands:
  shell "cmd..."      run a command as the diag user, print output
  root  "cmd..."      run a command as root via pkexec + pf-maintenance
  xfer LOCAL REMOTE   transfer a file (base64 over the tty, md5 verified)
  test [SIZE]         integrity probe with SIZE random bytes (default 1 MiB)

Credentials load from .claude/frame-credentials.json (gitignored); the port
auto-detects the first /dev/cu.usbmodem* device. Requires pyserial.

Transfer notes: the tty is put in -icanon -echo ixoff mode and `head -c`
consumes the stream; a READY handshake runs before streaming so head (not
bash readline) is the reader when bytes arrive. If a transfer is abandoned
mid-stream, head keeps eating input: recover with `shell` Ctrl-C or send
\\x03 then `stty sane`.
"""
import argparse
import base64
import glob
import hashlib
import json
import os
import re
import shlex
import sys
import time

import serial

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CRED_PATH = os.path.join(REPO, ".claude", "frame-credentials.json")
CHUNK = 2048
PAUSE = 0.003
XON = 0x11
XOFF = 0x13
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\]3008;[^\x07]*\x07")
SHELL_PROMPT = r"(?m:(?:^|[\r\n])[^\r\n]*[#$] $)"


def load_creds():
    with open(CRED_PATH) as f:
        return json.load(f)


def open_port(creds):
    port = creds.get("serial_port") or sorted(glob.glob("/dev/cu.usbmodem*"))[0]
    return serial.Serial(port, creds.get("baud", 115200), timeout=0.2)


def clean(text):
    return ANSI.sub("", text)


def read_until(ser, pattern, timeout, initial=""):
    deadline = time.monotonic() + timeout
    buf = initial
    while time.monotonic() < deadline:
        chunk = ser.read(8192).decode("utf-8", "replace")
        if chunk:
            buf += chunk
            if re.search(pattern, buf):
                return buf, True
    return buf, False


def login(ser, creds):
    ser.write(b"\n")
    ser.flush()
    out, ready = read_until(ser, rf"login:|{SHELL_PROMPT}", 3)
    if not ready:
        raise TimeoutError("serial console did not show login or shell prompt")
    if "login:" in out:
        ser.write(creds["username"].encode() + b"\n")
        ser.flush()
        out, ready = read_until(ser, rf"[Pp]assword:|{SHELL_PROMPT}", 5)
        if re.search(r"[Pp]assword:", out):
            ser.write(creds["password"].encode() + b"\n")
            ser.flush()
            _, ready = read_until(ser, SHELL_PROMPT, 8)
        if not ready:
            raise TimeoutError("serial login did not reach a shell prompt")
    time.sleep(0.3)
    ser.reset_input_buffer()


def run(ser, cmd, timeout=30):
    marker = "M" + str(time.time_ns())
    ser.write(f"{cmd}; echo {marker}$?\n".encode())
    ser.flush()
    out, _ = read_until(ser, marker + r"\d", timeout)
    m = re.search(marker + r"(\d+)", out)
    return (m.group(1) if m else "?"), clean(out)


def cmd_shell(args):
    creds = load_creds()
    ser = open_port(creds)
    try:
        login(ser, creds)
        for cmd in args.commands:
            rc, body = run(ser, cmd, timeout=args.timeout)
            print(f"$ {cmd}  [rc={rc}]")
            tail = body.split("\n")[-6:]
            print("\n".join(tail).strip())
    finally:
        ser.close()


def cmd_root(args):
    creds = load_creds()
    ser = open_port(creds)
    try:
        login(ser, creds)
        marker = "ROOTEND" + str(time.time_ns())
        complete = rf"(?m)^{marker}([0-9]+)\r?\n"
        wrapped = (
            "pkexec --disable-internal-agent /usr/local/sbin/pf-maintenance shell "
            f"-c {shlex.quote(args.command)} 2>&1; printf '\\n{marker}%s\\n' \"$?\""
        )
        deadline = time.monotonic() + 60
        ser.write(wrapped.encode() + b"\n")
        ser.flush()
        out, _ = read_until(ser, rf"{complete}|[Pp]assword[^\r\n:]*:", max(0, deadline - time.monotonic()))
        if not re.search(complete, out):
            if re.search(r"[Pp]assword[^\r\n:]*:", out):
                ser.write(creds["password"].encode() + b"\n")
                ser.flush()
            out, _ = read_until(ser, complete, max(0, deadline - time.monotonic()), initial=out)
        if not re.search(complete, out):
            raise TimeoutError("root command did not confirm completion within 60 seconds")
        print(clean(out))
    finally:
        ser.close()


def drain_flow(ser, deadline_s=30.0):
    end = time.time() + deadline_s
    while time.time() < end:
        if not ser.in_waiting:
            return True
        b = ser.read(ser.in_waiting)
        if XOFF in b:
            while time.time() < end:
                if ser.in_waiting:
                    if XON in ser.read(ser.in_waiting):
                        return True
                else:
                    time.sleep(0.002)
            return False
    return True


def transfer(ser, data, remote):
    payload = base64.b64encode(data)
    md5 = hashlib.md5(data).hexdigest()
    n = len(payload)
    rc, body = run(ser, "stty -icanon -echo ixoff")
    ser.write(f"echo READY; head -c {n} > {remote}.b64; echo XFERDONE\n".encode())
    ser.flush()
    _, hit = read_until(ser, r"READY", 5)
    if not hit:
        ser.write(b"\x03")
        raise RuntimeError("frame never said READY")
    time.sleep(0.15)
    t0 = time.time()
    for i in range(0, n, CHUNK):
        drain_flow(ser)
        ser.write(payload[i : i + CHUNK])
        ser.flush()
        time.sleep(PAUSE)
    dt = time.time() - t0
    print(f"streamed {n} bytes in {dt:.1f}s = {n / dt / 1024:.0f} KiB/s")
    _, hit = read_until(ser, r"XFERDONE", 120)
    if not hit:
        ser.write(b"\x03")
        ser.flush()
        time.sleep(0.5)
        ser.write(b"stty sane\n")
        raise RuntimeError("no XFERDONE marker (sent Ctrl-C + stty sane)")
    time.sleep(0.3)
    ser.reset_input_buffer()
    run(ser, "stty icanon echo")
    rc, body = run(
        ser,
        f"base64 -d {remote}.b64 > {remote}; wc -c {remote}; md5sum {remote}",
        timeout=120,
    )
    print(clean(body).split("\n")[-4:])
    if md5 not in body:
        raise RuntimeError("MD5 MISMATCH")
    print("MD5 MATCH")
    return md5


def cmd_xfer(args):
    creds = load_creds()
    with open(args.local, "rb") as f:
        data = f.read()
    ser = open_port(creds)
    try:
        login(ser, creds)
        transfer(ser, data, args.remote)
    finally:
        ser.close()


def cmd_test(args):
    creds = load_creds()
    data = os.urandom(args.size)
    ser = open_port(creds)
    try:
        login(ser, creds)
        transfer(ser, data, "/tmp/pf-probe.bin")
    finally:
        ser.close()


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("shell", help="run commands as diag")
    s.add_argument("commands", nargs="+")
    s.add_argument("--timeout", type=int, default=45)
    s.set_defaults(fn=cmd_shell)
    s = sub.add_parser("root", help="run one command as root via pf-maintenance")
    s.add_argument("command")
    s.set_defaults(fn=cmd_root)
    s = sub.add_parser("xfer", help="transfer a file to the frame")
    s.add_argument("local")
    s.add_argument("remote")
    s.set_defaults(fn=cmd_xfer)
    s = sub.add_parser("test", help="serial integrity probe")
    s.add_argument("--size", type=int, default=1024 * 1024)
    s.set_defaults(fn=cmd_test)
    args = p.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
