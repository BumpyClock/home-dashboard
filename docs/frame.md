# PF1007L GPU build

The `frame` Cargo feature uses Slint's LinuxKMS backend, FemtoVG OpenGL ES
renderer, and libinput. It renders directly through DRM/KMS and Mesa. The
Mali-400 uses Mesa's Lima driver. This build does not include Skia, Vulkan,
or a software renderer. The default `desktop` feature retains Slint's
desktop defaults for `cargo run`.

## Frame dependencies

On the frame's Arch Linux ARM armv7h installation:

```sh
pacman -Syu
pacman -S --needed mesa libglvnd libdrm fontconfig ttf-dejavu libinput libxkbcommon xkeyboard-config
```

Pacman installs libinput's dependencies. The frame kernel must already
provide the display, Lima GPU, and touchscreen drivers. Existing libinput
touch calibration in `/etc/udev/rules.d/` applies to this app too.

Install a system font even though the UI fonts are embedded. Slint 1.18.1's
fontique dependency initializes generic system font families before the
embedded fonts can be used. On a fontless image it panics with
`Result::unwrap()` and `Err(NoMatch)`. Installing `ttf-dejavu` resolved this
on the frame. Check that `fc-match sans-serif`, `fc-match serif`, and
`fc-match monospace` each return a font before launching.

The frame build omits libseat. Run the app as root for direct access to the
DRM and input devices. USB serial remains available through `ttyGS0` while
the app owns the display.

## Cross-build on Linux or WSL

Install Rust 1.92 or later, Python 3, the ARM hard-float GCC cross compiler,
and pkg-config. Add the Rust target and fetch the locked dependencies:

```sh
rustup target add armv7-unknown-linux-gnueabihf
cargo fetch --locked --target armv7-unknown-linux-gnueabihf
```

Prepare an Arch Linux ARM armv7h sysroot outside this repository. Its
`usr/include` and `usr/lib` must match the frame's installed packages,
including libc startup objects, Mesa/GBM, libinput, libudev, and fontconfig.
Preserve symlinks. Do not use the host's x86 libraries or an AArch64 sysroot.

```sh
PF_SYSROOT=/path/to/armv7h-sysroot \
PF_BUILD_DIR="$HOME/.cache/photoframe-proto-build" \
bash scripts/build-frame.sh
```

The script builds with `--locked --release --no-default-features --features
frame`, targets Cortex-A7 with NEON, and keeps all generated files outside the source
repository. It prints the executable's path. Copy that executable and
`assets/photos/` to the frame, for example under `/opt/photoframe-proto/`.
Fonts and UI assets are embedded; photos are loaded from disk.

Rust can warn that the `neon` target feature is unstable on this target.
The compiler setting is not evidence that the JPEG decoder uses ARMv7 NEON
intrinsics or that decoding is faster.

Frame builds enable the pinned Slint 1.18.1 image anti-aliasing correction
and FemtoVG 0.27.0 quarter-turn text correction by default. The latter keeps
rotated solid text at its upright weight without adding render layers.
The build host needs Python 3 and GNU `patch` in addition to the cross compiler.
To compare without either correction:

```sh
PF_SYSROOT=/path/to/armv7h-sysroot \
PF_BUILD_DIR="$HOME/.cache/photoframe-proto-build" \
PF_FEMTOVG_IMAGE_AA_FIX=0 \
PF_FEMTOVG_QUARTER_TURN_FIX=0 \
bash scripts/build-frame.sh
```

The switches are independent: disabling image AA leaves the text correction
enabled, and disabling the text correction leaves image AA enabled.
The default build needs an already fetched Cargo dependency set for offline
metadata and lockfile resolution. The helper verifies pinned source and patch
SHA-256 values, copies the selected crates and app build inputs to a unique
external directory, and applies the corrections there. Cargo updates only
that copy's lockfile. The checkout's `Cargo.toml`, `Cargo.lock`, and registry
sources remain unchanged. Desktop `cargo run` does not apply these patches.
See [rotated text details](orientation.md#text-rendering). Speed benefits
require separate measurements.

## Reserve memory for display buffers

The development frame's original kernel reserved 16 MiB of contiguous
memory (CMA). Slint's XRGB8888 scanout buffers need about 3.91 MiB each at
800x1280, in addition to the console framebuffer and other buffers. With
16 MiB, the app failed with `DRM_IOCTL_MODE_CREATE_DUMB` and
`Cannot allocate memory`. A 64 MiB default CMA pool fixed the allocation
failure with the same kernel, initramfs, and app binary.

Check the installed frame before changing its boot profile:

```sh
grep -E '^Cma(Total|Free):' /proc/meminfo
```

If it already reports `CmaTotal: 65536 kB`, retain that profile. For the
original 16 MiB configuration, create a new profile from the actual
installed five-file boot bundle. Keep its kernel and initramfs together;
the initramfs defines the installed storage layout and root-image pins.
Do not substitute a bundle from a different SD image.

This kernel has `CONFIG_CMDLINE_FORCE=y`, so adding `cma=64M` to U-Boot's
arguments has no effect. The profile recipe adds a reusable 64 MiB default
CMA pool under the device tree's `/reserved-memory`. It preserves the
existing board properties, kernel, initramfs, and storage layout, then
updates the launcher's DTB size/hash pins and bundle checksums. It rejects
trees with an existing reserved-memory policy instead of overwriting it.

On a Debian/Ubuntu or WSL host, install `device-tree-compiler` for `fdtget`
and `fdtput`, and `u-boot-tools` for `mkimage`. Keep the input bundle and
generated output outside the checkout. The output directory must be new.

```sh
python3 scripts/make-cma-profile.py \
  --bundle /work/installed-frame-bundle \
  --output /work/frame-cma64 \
  --profile-id arch-usb-slint-cma64-r1
```

The command prints its new `working-control-` build ID. Copy the five-file
output to an external Windows work directory, then use the USB tools from
the sibling [PF1007L Linux setup repository](https://github.com/BumpyClock/pf1007l-archlinux-arm).
From that repository in PowerShell, validate first:

```powershell
./tools/pf-usb-install.ps1 -BundlePath ../pf1007l-work/frame-cma64 -ProfileId arch-usb-slint-cma64-r1 -ValidateOnly
```

Install and select using the exact build ID printed by the generator:

```powershell
$frameCredential = Get-Credential
$serialPort = Read-Host 'Actual frame COM port'
$cmaBuild = Read-Host 'Build ID printed by make-cma-profile.py'
./tools/pf-usb-install.ps1 -Port $serialPort -Credential $frameCredential -BundlePath ../pf1007l-work/frame-cma64 -ProfileId arch-usb-slint-cma64-r1 -Select -BuildId $cmaBuild
```

Use the frame's actual COM port. Close other serial terminals before the
installer opens it. The installer selects the trial without rebooting;
keep the existing recovery dispatcher and profile. After checking its
receipt, reconnect an interactive serial terminal and run:

```sh
pkexec --disable-internal-agent /usr/local/sbin/pf-maintenance shell -c 'sync; systemctl reboot'
```

After USB returns, open the maintenance root shell and check the pool and
healthy-boot record:

```sh
pkexec --disable-internal-agent /usr/local/sbin/pf-maintenance shell
grep -E '^Cma(Total|Free):' /proc/meminfo
cat /var/lib/pf-maintenance/healthy
cat /proc/sys/kernel/random/boot_id
exit
```

Confirm `CmaTotal: 65536 kB`; the healthy record's `boot_id` must match the
current boot ID, and its `build` must match the generator's printed ID.
A failed trial returns to the retained recovery profile on its next boot.
Use a new profile ID for subsequent trials; installed profiles are immutable.

## Restore GPU scaling and temperature limits

The tested frame runs kernel 7.2.10. Its original A33 GPU operating-point
table contains 144, 240, and 384 MHz. `--gpu-devfreq` restores the link to
that existing table; it does not invent voltages or add frequencies.
Mainline `simple_ondemand` selects among those points with a 50 ms polling
interval. The stock Android thermal policy allowed 384 MHz below 85 °C,
240 MHz at 85 °C, and 144 MHz at 95 °C.

Build the temperature module from the exact prepared kernel source,
configuration, and `Module.symvers` that produced the installed kernel:

```sh
PF_KERNEL_BUILD=/work/prepared-frame-kernel \
PF_THERMAL_BUILD_DIR=/work/frame-thermal \
bash scripts/build-thermal.sh
```

The script builds `sun4i-gpadc-iio.ko` outside the source and kernel input
trees and checks that the kernel configuration and symbol table remain
unchanged. The tested module passed symbol-import checks and loaded on the
frame; live chip-temperature readings were 46–50 °C. Do not reuse a module
after changing the kernel build.

Copy the module, `scripts/gpu-thermal-limit.sh`, `deploy/pf-thermal.conf`, and
`deploy/pf-gpu-thermal.service` to a staging directory on the frame. In a
root shell, check the module's `vermagic` against `uname -r`, then install:

```sh
uname -r
modinfo -F vermagic /var/tmp/pf-thermal/sun4i-gpadc-iio.ko
install -d "/usr/lib/modules/$(uname -r)/updates"
install -m 0644 /var/tmp/pf-thermal/sun4i-gpadc-iio.ko "/usr/lib/modules/$(uname -r)/updates/"
install -m 0755 /var/tmp/pf-thermal/gpu-thermal-limit.sh /usr/local/sbin/pf-gpu-thermal-limit
install -m 0644 /var/tmp/pf-thermal/pf-thermal.conf /etc/modules-load.d/pf-thermal.conf
install -m 0644 /var/tmp/pf-thermal/pf-gpu-thermal.service /etc/systemd/system/pf-gpu-thermal.service
depmod -a
modprobe sun4i_gpadc_iio
systemctl daemon-reload
systemctl enable pf-gpu-thermal.service
```

Create the GPU trial with the profile generator:

```sh
python3 scripts/make-cma-profile.py \
  --bundle /work/matching-frame-bundle-without-reserved-memory \
  --output /work/frame-cma64-gpu \
  --profile-id arch-usb-slint-cma64-gpu-r1 \
  --gpu-devfreq
```

The input must be a verified five-file bundle whose kernel and initramfs
match the installed frame. The generator rejects an existing
`/reserved-memory` policy, including its own earlier CMA output. Use the
matching original DTB bundle; do not remove a reserved-memory policy by
hand. The command preserves the kernel and initramfs, adds 64 MiB CMA, and
updates the DTB pins and bundle checksums. Add `--accelerometer` only when
the PF1007L SC7A20 sensor is also required. Validate, install, and confirm
the healthy-boot record using the preceding profile procedure, with the
new bundle, profile ID, and generated build ID.

After that boot, start and inspect the temperature service:

```sh
systemctl start pf-gpu-thermal.service
systemctl status pf-gpu-thermal.service --no-pager
cat /sys/class/devfreq/1c40000.gpu/available_frequencies
cat /sys/class/devfreq/1c40000.gpu/governor
cat /sys/class/devfreq/1c40000.gpu/max_freq
cat /sys/class/devfreq/1c40000.gpu/cur_freq
journalctl -u pf-gpu-thermal.service --no-pager -n 20
```

The service uses `Type=notify` and starts only when the devfreq control
exists. It reads `cpu-thermal`, applies the stock frequency ceilings with
2 °C hysteresis, and checks the ceiling every 0.5 seconds. Missing,
malformed, or out-of-range temperature data limits the GPU to 144 MHz.
On exit, the guard attempts to restore the 144 MHz ceiling. Invalid readings
emit a warning and update systemd's status to the fallback ceiling; valid
readings emit a recovery message. Exiting logs whether the fallback write
succeeded. The thermal harness passed 22 cases covering these transitions.
Keep this service enabled when the 384 MHz operating point is available.
The GPU was observed runtime-suspended while the dashboard was idle.
Leave frequency selection to `simple_ondemand`; do not force a fixed
384 MHz clock to keep the app responsive.

If the guard is inactive, check its condition result, current ceiling, and
journal together:

```sh
systemctl show pf-gpu-thermal.service -p ActiveState -p SubState -p Result -p ConditionResult -p StatusText
cat /sys/class/devfreq/1c40000.gpu/max_freq
journalctl -u pf-gpu-thermal.service --no-pager -n 40
```

`ConditionResult=no` means systemd skipped the unit's startup condition.
Confirm that the devfreq path now exists before starting the service.
Do not infer a safe ceiling from an inactive status; read `max_freq`.

## Run and inspect

From a USB serial or SSH root shell on the frame:

```sh
SLINT_BACKEND=linuxkms-femtovg \
GALLIUM_DRIVER=lima \
FRAME_PHOTOS=/opt/photoframe-proto/assets/photos \
/opt/photoframe-proto/photoframe_proto
```

The backend prints `Using FemtoVG OpenGL renderer` when it creates the
renderer. Set `SLINT_DEBUG_PERFORMANCE=refresh_lazy,console` for Slint's
rendering statistics. `GALLIUM_DRIVER=lima` selects the Mali-400 driver;
inspect the process's DRM descriptors to confirm that it opens the Lima
render node `/dev/dri/renderD128`. Renderer selection alone does not prove
GPU use.

Measured on the development frame: the CMA profile reports 64 MiB, the
ARMv7 binary renders at 800x1280 with FemtoVG and Lima, and the
touchscreen opens Settings. The dashboard remains visible after startup.

The frame build enables caches for the Agenda heading and static rows,
navigation button layouts, and Settings headings and static card bodies.
Moving navigation notches, the live Header and Chores, and animated Settings outlines
draw directly. `FRAME_CACHE_MAIN_CONTENT=0` disables the first group;
`FRAME_CACHE_DASHBOARD=0` disables the summary and weather caches.
Photo transitions default to Slide; choose Fade in the Settings tab. Frame builds default to baked caption ramps;
`FRAME_PHOTO_SCRIM=svg` restores the overlay, and `baked` forces baking in a
desktop build. The UI keeps the SVG overlay until the first baked batch
publishes. Raster failure retains the previous photo batch and permits a
retry. Actual GPU readback confirmed the baked pixels within 2/255 in the
caption region, with identical upper photo pixels. These defaults are installed in verified release `b27b4b6f`. Its executable
SHA-256 is `b27b4b6fe0cf4a8578cab797a24a6b8f2a29e8510098847cde16b7ef6f7a20e2`.
Checkpoint `9e34868` remains available locally for rollback.
See [performance measurements](performance.md) for matched comparisons
and the external JSON capture workflow.

Slint chooses the connected DRM display, so `/dev/dri/card0` being the Lima
GPU and `/dev/dri/card1` being the display does not require a hardcoded card
number. Use `SLINT_DRM_OUTPUT=list` or `SLINT_DRM_MODE=list` to inspect the
detected connector and modes.

The panel's native mode is 800x1280, so the frame starts in the portrait
reflow; `FRAME_SIZE=800x1280` only changes desktop preview windows. For
landscape, `SLINT_KMS_ROTATION=90` rotates the output, but touch must use a
matching libinput calibration matrix. Do not rotate an already calibrated
touch configuration a second time.

## Persistent app service

[deploy/photoframe-proto.service](../deploy/photoframe-proto.service) runs
`/opt/photoframe-proto/current/photoframe_proto`, loads photos from
`/var/lib/photoframe-proto/photos`, and stores settings in
`/var/lib/photoframe-proto/settings.json`. Prepare the `current` release and
verify its foreground launch before enabling the service. Install the
restoration helper in each release before selecting it:

```sh
install -d -m 0755 /opt/photoframe-proto/current/scripts
install -m 0755 /var/tmp/pf-release/restore-backlight.sh /opt/photoframe-proto/current/scripts/restore-backlight.sh
```

The frame's hardware boot unit must use `Type=oneshot` and
`RemainAfterExit=yes`, and its script must support `PF_CONSOLE_GETTY=0`.
Use the maintained runtime files from
[the PF1007L setup repository](https://github.com/BumpyClock/pf1007l-archlinux-arm/tree/main/runtime/qualified)
when upgrading an older base. Otherwise, starting the app can rerun hardware
initialization and its console-login step can stop the dashboard.

Stage the app unit and `deploy/pf-console-boot.service.d/80-photoframe-proto.conf`
on the frame, then install them from a root shell:

```sh
grep -q PF_CONSOLE_GETTY /usr/local/sbin/pf-console-boot
install -d -m 0755 /var/lib/photoframe-proto /etc/systemd/system/pf-console-boot.service.d
install -m 0644 /var/tmp/pf-release/80-photoframe-proto.conf /etc/systemd/system/pf-console-boot.service.d/80-photoframe-proto.conf
install -m 0644 /var/tmp/pf-release/photoframe-proto.service /etc/systemd/system/photoframe-proto.service
systemctl daemon-reload
systemctl enable photoframe-proto.service
systemctl restart photoframe-proto.service
systemctl status photoframe-proto.service --no-pager
journalctl -u photoframe-proto.service --no-pager -n 30
```

The unit orders startup after the console-boot and GPU-temperature
services and requires both to succeed. Hardware boot finishes once and
remains active; app restarts reuse it. The thermal unit also waits for
that completion before evaluating its GPU-path condition. The app drop-in suppresses the boot
script's console-login action while the dashboard owns tty1. It conflicts with `getty@tty1.service` so the local VT console
cannot paint over the dashboard. USB serial on `ttyGS0` remains available;
do not disable its getty. To return to the local VT console:

```sh
systemctl stop photoframe-proto.service
systemctl start getty@tty1.service
```

The SC7A20 accelerometer supports automatic UI and touch rotation after
calibration; see [rotation setup](orientation.md). Keep backend rotation
unset or zero. The UI controller preserves the native display mode.

Backend details: [Slint LinuxKMS documentation](https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backend_linuxkms/).

## Backlight and idle behavior

Frame builds observe readable touchscreen, keyboard, and mouse event
devices independently of Slint. They do not grab input. Three minutes
without human activity dims the backlight to 20%; input restores 65%.
Rendering, weather, the clock, and automatic photo changes do not reset
this timer. No activity reader means idle dimming stays disabled.
Brightness writes run on a coalescing worker, outside the UI thread.

Override defaults with a systemd drop-in:

```ini
[Service]
Environment=FRAME_IDLE_DIM_SECONDS=180
Environment=FRAME_IDLE_BRIGHTNESS=20
Environment=FRAME_DAY_BRIGHTNESS=65
```

Set `FRAME_IDLE_DIM_SECONDS=0` to disable idle dimming. Night behavior is
disabled unless both `FRAME_NIGHT_START` and `FRAME_NIGHT_END` are set,
for example `22:00` and `06:00`. Within that window, idle brightness uses
`FRAME_NIGHT_BRIGHTNESS` (10%) and automatic slides pause. Interaction
still restores the day brightness. The window is interpreted in the
configured time zone (Settings tab; `System` follows the OS zone), and
hardware schedules follow the same zone; `FRAME_NOW` only shifts the
dashboard preview. Custom night endpoints
inside a repeated daylight-saving hour can remain stale until input or
the next boundary; use endpoints outside that hour.

`FRAME_BACKLIGHT_PATH` can select a device when multiple system backlights
exist. The stop helper accepts only a member of `/sys/class/backlight`.
Graceful SIGTERM/SIGINT shuts down the UI, flushes pending settings, and
restores day brightness. `ExecStopPost` also restores it after a crash or
forced termination; this requires the helper above in the selected release.

Photos are copied to `/var/lib/photoframe-proto/photos` (never into the repo).
Create that directory and copy `.jpg` or `.jpeg` files into it, then restart
the app to rescan. An empty folder uses the selected release's bundled
`assets/photos`. Files with no EXIF date come first; dated files follow in
ascending `DateTimeOriginal` order. Captions show the month and year when
available, and EXIF rotation and mirroring are applied before cropping.

The Photos tab opens a scrolling thumbnail grid. Tap a photo or
**Start slideshow** to open it full-screen. Swipe left for the next photo,
swipe right for the previous photo, or tap to return. Swipes wrap at both
ends of the collection and do not dismiss the slideshow. Slides are
the default transition in both views. If a destination photo is still loading,
the current photo stays visible until its full-screen image is ready. Further
swipes select the latest destination without displaying an empty image.
Full-screen photos have no bottom
gradient. The Home card retains its caption gradient. Full motion permits automatic advances
every 20 seconds in the screensaver and every two minutes on Home; Calm/Off
and configured night hours pause them. Rotation retains the screensaver,
resizes its photos for the new orientation, and keeps touch aligned.

Metadata scanning runs off the UI thread. Decoded images include the actual
transition pair and a small prefetch window, plus at most 64 nearby grid
thumbnails. The grid creates only nearby tiles. File paths and captions grow
with the library; decoded image memory stays bounded. A single worker allows
only one queued image batch and rejects results from outdated requests.
While the slideshow is open, its requested full-screen photo loads first;
hidden Home-card prefetch is paused.
Compressed JPEG input is capped at 32 MiB, source pixels at 64 MiB, resize
intermediates at 32 MiB, and each output at 16 MiB with dimensions at most
4096. Decode attempts beyond these limits stop without a retry loop.
Three minutes of idleness dims the backlight; two minutes opens the
screensaver (delay in the Settings tab; Never disables automatic entry), and any
tap closes it. Benchmark mode suppresses automatic screensaver entry.

Photos use EXIF orientation and exact physical-size thumbnails. The CPU
thumbnail cache retains at most 16 entries or 16 MiB and keys files by
canonical path, modification time, length, size, and scrim mode. A size
roundtrip can reuse existing pixels. The playlist is scanned at startup;
restart after adding or removing files. Weather requests use a 20-second total timeout
and a 64 KiB body limit. Settings writes are debounced for 250 ms, then
synced and atomically replaced; a power loss before that write can lose
the latest change, while the previous file remains intact.
