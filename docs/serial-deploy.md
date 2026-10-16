# macOS build and serial deploy runbook

How this repository gets built and deployed to the PF1007L frame from a
Mac, with no shared network between the Mac and the frame. Everything goes
through the frame's USB serial console (`ttyGS0`, exposed as
`/dev/cu.usbmodem*`) and a Docker cross-build container.

## Frame access

- Serial console: 115200 8N1 over USB CDC; the device is
  `/dev/cu.usbmodem*` (baud is nominal — data moves at USB speed,
  ~500 KiB/s sustained).
- Login as the `diag` user. Credentials are **not** in the repository:
  - local file: `.claude/frame-credentials.json` (`.claude/` is gitignored)
  - 1Password item: *PF1007L frame serial console*
- Root: same password, via
  `pkexec --disable-internal-agent /usr/local/sbin/pf-maintenance shell -c '<cmd>'`.
- SSH is disabled; the frame's WiFi (10.149.100.0/24) is not reachable
  from the Mac, so serial is the only channel.
- `root` returns after about 60 s. Run longer jobs detached
  (`setsid -f sh -c '...' </dev/null >/dev/null 2>&1`) and read their
  output later as root: files written under `pf-maintenance` are not
  readable by `diag`.
- Only one process can hold the port. `screen /dev/cu.usbmodem*` left
  attached blocks scripted access — close it first.

## Tooling

`scripts/frame-console.py` (needs `pyserial`: `pip install pyserial`, or
run it as `uv run --with pyserial python3 scripts/frame-console.py ...`)
drives the console:

```sh
python3 scripts/frame-console.py shell "uname -a"          # as diag
python3 scripts/frame-console.py root  "systemctl status photoframe-proto.service"
python3 scripts/frame-console.py test                      # 1 MiB integrity probe
python3 scripts/frame-console.py xfer local.bin /tmp/remote.bin
```

Transfer method: base64 streamed through the login shell with the tty in
`-icanon -echo ixoff` mode, consumed by `head -c`, verified with md5 on
the frame. A READY handshake runs before streaming so `head` (not bash
readline) is reading when bytes arrive. If a transfer is abandoned
mid-stream, `head` keeps consuming input; recover by sending Ctrl-C
(`\x03`) then `stty sane`.

## Cross-build on macOS

`scripts/build-frame-container.sh` does everything: creates or reuses the
`pf-build` Debian bookworm container (amd64 under Rosetta), installs the
ARM hard-float cross GCC + Rust, downloads an Arch Linux ARM armv7h
sysroot, and runs `scripts/build-frame.sh` inside it. The binary lands in
`~/.cache/pf-build-out/photoframe_proto` (override with `PF_OUT_DIR`).

The sysroot is built over HTTPS from `fl.us.mirror.archlinuxarm.org` using
the package versions pinned in the script's `PKGS` array. These must match
the frame's installed packages — check with
`frame-console.py shell "pacman -Q glibc mesa libinput ..."` after
upgrading the frame, and confirm the sysroot's glibc max symbol
(printed by the build) does not exceed the frame's glibc.

Every package is checked against the SHA256 in the mirror's repo database
(`scripts/resolve-alarm-packages.py`) before it is unpacked; a mismatch stops
the build. The database is unsigned, so this protects the transfer but still
trusts the mirror. If a pinned version is no longer in the database, the
script prints a `NOTE` and uses the mirror's current version. Packages live
under `armv7h/core/` and `armv7h/extra/`, and filenames URL-encode `+` and
`:`. Some mirrors (including `hu.`) do not answer HTTPS.

## Deploy

The frame keeps immutable releases under `/opt/photoframe-proto/releases/`
with a `current` symlink. Deploy as the sha256-prefixed release:

```sh
SHA=$(shasum -a 256 ~/.cache/pf-build-out/photoframe_proto | cut -c1-8)
python3 scripts/frame-console.py xfer ~/.cache/pf-build-out/photoframe_proto /tmp/photoframe_proto.new
python3 scripts/frame-console.py root "R=/opt/photoframe-proto/releases/$SHA; \
  install -d -m 0755 \$R/scripts && \
  install -m 0755 /tmp/photoframe_proto.new \$R/photoframe_proto && \
  install -m 0755 /opt/photoframe-proto/current/scripts/restore-backlight.sh \$R/scripts/ && \
  ln -sfn /opt/photoframe-proto/releases/e55ec6a2/assets \$R/assets && \
  sha256sum \$R/photoframe_proto"
# verify the printed hash matches the local one, then cut over:
python3 scripts/frame-console.py root "ln -sfn /opt/photoframe-proto/releases/$SHA /opt/photoframe-proto/current && \
  systemctl restart photoframe-proto.service && sleep 4 && systemctl is-active photoframe-proto.service && \
  journalctl -u photoframe-proto.service --no-pager -n 12"
```

`restore-backlight.sh` is required in every release (`ExecStopPost` runs
it). The shared `assets` tree currently lives in release `e55ec6a2`;
link to it rather than copying. A healthy start prints
`Using FemtoVG OpenGL renderer` and `Rendering at 800x1280`.

Rollback: repoint `current` at the previous release and restart. Old
releases are never modified.

## Frame facts (measured 2026-10-14)

- Arch Linux ARM armv7h, kernel `7.2.10-pf-console-r1`, glibc 2.43,
  Mesa 26.2.4 with Lima (Mali-400), 64 MiB CMA, panel 800x1280.
- The dashboard runs as `photoframe-proto.service` (enabled), photos in
  `/var/lib/photoframe-proto/photos`, settings in
  `/var/lib/photoframe-proto/settings.json`.
- USB serial stays available while the app owns the display; do not
  disable the `ttyGS0` getty.
