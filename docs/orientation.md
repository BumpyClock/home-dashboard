# Automatic rotation

Measured on the development frame: the SC7A20 at TWI1 address `0x19` bound
to the Linux IIO driver, reported the name `sc7a20`, and returned raw XYZ
readings. Linux 7.2.10 supports `silan,sc7a20` through its ST accelerometer
driver. The driver checks register `0x0f` for identity value `0x11` before
exposing an IIO device.

Frame builds can rotate the dashboard and touch coordinates together using
a calibrated SC7A20. Desktop builds keep their configured orientation.
Missing calibration, unavailable sensor reads, and flat or diagonal poses
hold the current UI orientation. The app does not restart when it rotates.

## Calibrate and enable

First install the matching modules and sensor-enabled boot profile
described below.

Record XYZ with the dashboard's portrait top edge upright, then with the
frame turned clockwise by 90, 180, and 270 degrees. Each stable reading
must be close to one gravity unit. Measured on the development frame:

| Physical pose | Dominant gravity axis | UI compensation |
|---|---|---|
| Portrait upright | `-y` | 0 degrees |
| Clockwise landscape | `+x` | 270 degrees |
| Upside-down portrait | `+y` | 180 degrees |
| Opposite landscape | `-x` | 90 degrees |

Verify the signs on your hardware before using this example. Install the
calibration from a maintenance root shell:

```sh
install -d -m 0755 /var/lib/photoframe-proto
cat > /var/lib/photoframe-proto/orientation.json <<'JSON'
{"upright_axis":"-y","clockwise_axis":"+x"}
JSON
chmod 0644 /var/lib/photoframe-proto/orientation.json
systemctl restart photoframe-proto.service
journalctl -u photoframe-proto.service --no-pager -n 30
```

Both axes must be signed `x` or `y` axes and must be orthogonal. The file
is limited to 4096 bytes. `FRAME_ORIENTATION_CONFIG` selects another file;
`FRAME_AUTO_ROTATE=0` disables the controller. Configuration is read at
startup. Keep `SLINT_KMS_ROTATION` unset or `0`; the controller rejects a
second backend rotation.

The worker finds the sensor by its exact IIO name, selects its advertised
50 Hz sample rate, and reads poses at 2 Hz outside the UI thread. A new
orientation requires three consistent samples. It rejects acceleration
outside 0.85–1.15 gravity units, less than 0.50 gravity units in the screen
plane, and directions near diagonals. Normalizing that planar direction
supports tilted stands while a flat frame holds its orientation.
Sequential XYZ reads are not synchronized, so movement can
delay a rotation. Failed reads cancel pending changes and retain the last
applied orientation.

A touch contact delays the change until the pose is still valid and the
finger is lifted. The observer queries current contacts at startup and
after lost input events. Unknown contact state blocks rotation until it
can be recovered; a stopped pointer reader blocks it until app restart.
A rotation restores day brightness and restarts the idle
timer. The native DRM mode stays 800×1280. Slint rotates a centered scene,
swaps its logical width and height for landscape, and applies the inverse
transform to touch coordinates. There is no added full-screen cache layer
or rotation animation. Existing native touchscreen calibration stays in
place.

After installation, check all four poses and tap Settings in each. Hold a
finger down while turning, then release it to check deferred rotation.
USB serial remains available throughout. Startup and accepted angles are
logged with the `[orientation]` prefix.

## Text rendering

FemtoVG 0.27.0 normally switches rotated text from its glyph atlas to outline
rendering. On the development frame, this made the greeting, date, chores,
and photo captions look heavier in every nonzero orientation.

Frame builds apply [a pinned patch](../patches/femtovg-0.27.0-quarter-turn-atlas.patch)
that retains the existing atlas for solid text up to 92 physical pixels
under unit 90-, 180-, and 270-degree rotations. It snaps the text draw matrix
to the exact quarter turn and rounds its physical translation, then restores
the original transform. Larger text, gradients, scaled or arbitrary-angle
transforms keep their previous rendering path. Font weights stay unchanged,
and the correction adds no render layers.

Measured: framebuffer comparisons showed the upright image unchanged across
all 1,024,000 pixels and corrected strokes in the other three orientations.
The user confirmed normal text and working touch in all four orientations.
Six focused renderer tests pass with both text rasterizers. This establishes
appearance and transform behavior; it does not establish a frame-rate gain.

`PF_FEMTOVG_QUARTER_TURN_FIX=0` disables this correction in
`scripts/build-frame.sh`; it is independent of `PF_FEMTOVG_IMAGE_AA_FIX`.
See the [frame build instructions](frame.md).

## Build the raw IIO modules

Use the exact prepared kernel build for the running frame, including its
configuration, generated headers, and `Module.symvers`. For the qualified
`7.2.10-pf-console-r1` kernel, IIO, I2C, and regmap are built in; IIO buffers
and triggers are disabled. The module recipe retains this configuration
and builds the raw sysfs interface without interrupt or buffered sampling.

Install an ARM hard-float GCC cross compiler and GNU make on the Linux or
WSL build host. Prepare the matching kernel through the
[PF1007L kernel recipe](https://github.com/BumpyClock/pf1007l-archlinux-arm/blob/main/kernel/README.md),
then build outside both source repositories and the prepared kernel tree:

```sh
PF_KERNEL_BUILD=/work/qualified-kernel-build \
PF_SENSOR_BUILD_DIR=/work/orientation-build \
bash scripts/build-accelerometer.sh
```

The script copies the unmodified driver sources from the prepared kernel's
`source` tree and explicitly separates kernel inputs, external module
sources, and outputs. It checks that the kernel configuration and export
table remain unchanged after the build. Outputs are five modules plus
`SHA256SUMS` under `/work/orientation-build/modules/`:

- `regmap-i2c.ko`
- `st_sensors.ko`
- `st_sensors_i2c.ko`
- `st_accel.ko`
- `st_accel_i2c.ko`

Measured: these modules compile against the qualified kernel, modpost
reports no unresolved symbols, and all 68 module imports resolve against
the kernel and module stack. Their vermagic is
`7.2.10-pf-console-r1 SMP ARMv7 p2v8`. The kernel disables
`CONFIG_MODVERSIONS`, so this build has no symbol CRC protection. Matching
the release string alone is insufficient; retain the exact kernel build
and its module ABI.

## Enable the sensor's I2C controller

The original Linux device tree disables TWI1 at `/soc/i2c@1c2b000`.
A reviewed boot-profile DTB must enable that controller with its PH4/PH5
I2C pins and add an `accelerometer@19` child with these properties:

```dts
accelerometer@19 {
    compatible = "silan,sc7a20";
    reg = <0x19>;
};
```

Raw polling does not need an interrupt assignment. Do not add guessed
power supplies or regulator voltages. Preserve the working display, touch,
Wi-Fi, CMA, and storage properties. The profile recipe enables TWI1 and
adds this sensor while retaining the existing PH4/PH5 pinmux:

```sh
python3 scripts/make-cma-profile.py \
  --bundle /work/original-installed-frame-bundle \
  --output /work/frame-cma64-accelerometer \
  --profile-id arch-usb-slint-accel-r1 \
  --cma-mib 64 --accelerometer
```

Use the saved pre-CMA bundle from this same installation. It must contain
the exact working kernel and initramfs, with the five files expected by
the recipe. The generator rejects an existing reserved-memory node, so
an already patched CMA bundle is not a valid input. Its generated DTB
contains both the 64 MiB pool and sensor changes. Host dependencies are
`fdtget`/`fdtput` from `device-tree-compiler`, and `mkimage` from
`u-boot-tools`.

The generator updates the boot launcher's DTB pins without changing the
kernel, initramfs, or existing board properties apart from TWI1's status.
Install its new immutable profile using the printed build ID through the
[protected USB update procedure](frame.md#reserve-memory-for-display-buffers).

## Install and inspect

Copy the five matching modules to an external staging directory on the
frame. From its maintenance root shell, install them under the running
kernel's module directory, run `depmod`, and load the top-level driver:

```sh
release=$(uname -r)
install -d -m 0755 "/usr/lib/modules/$release/extra/orientation"
install -m 0644 /path/to/staged-modules/*.ko "/usr/lib/modules/$release/extra/orientation/"
depmod -a "$release"
modprobe st_accel_i2c
```

`modprobe` loads the regmap and ST sensor dependencies. The enabled
controller also needs the existing `i2c_mv64xxx` module. Do not replace
unrelated kernel modules. This kernel disables module unloading; changing
a loaded sensor driver requires a reboot.

Find the sensor by name rather than assuming an IIO device number:

```sh
sensor=
for candidate in /sys/bus/iio/devices/iio:device*; do
    [ -f "$candidate/name" ] || continue
    if [ "$(cat "$candidate/name")" = sc7a20 ]; then
        sensor=$candidate
        break
    fi
done
test -n "$sensor"
cat "$sensor/sampling_frequency_available"
echo 50 > "$sensor/sampling_frequency"
cat "$sensor/in_accel_x_raw" "$sensor/in_accel_y_raw" "$sensor/in_accel_z_raw"
cat "$sensor/in_accel_x_scale" "$sensor/in_accel_y_scale" "$sensor/in_accel_z_scale"
```

The driver defaults to 1 Hz. Each raw axis read enables the sensor and
waits two sample periods, so a complete XYZ read takes about six seconds
at that default. At 50 Hz the three waits total about 120 ms. Polling at
2 Hz on a worker thread is sufficient for orientation; avoid blocking the
UI thread. XYZ reads are sequential, not one synchronized buffered sample.

For persistent driver loading, install [the module list](../deploy/pf-orientation.conf)
as `/etc/modules-load.d/pf-orientation.conf`. The qualified hardware boot
script loads `i2c_mv64xxx`; the sensor driver binds when that controller
appears. The matching sensor DTB and modules must be installed before
enabling the app controller.
