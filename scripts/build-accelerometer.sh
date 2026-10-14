#!/usr/bin/env bash
set -euo pipefail

: "${PF_KERNEL_BUILD:?Set PF_KERNEL_BUILD to the exact prepared frame kernel build}"
kernel_build=$(realpath "$PF_KERNEL_BUILD")
kernel_source=$(realpath "$kernel_build/source")
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
work_dir=$(realpath -m "${PF_SENSOR_BUILD_DIR:-$HOME/.cache/photoframe-orientation-build}")
case "$work_dir/" in "$source_dir/"*) echo 'PF_SENSOR_BUILD_DIR must be outside the source repository' >&2; exit 1;; esac
case "$work_dir/" in "$kernel_build/"*|"$kernel_source/"*) echo 'PF_SENSOR_BUILD_DIR must be outside the prepared kernel and its source' >&2; exit 1;; esac

for config in CONFIG_MODULES=y CONFIG_IIO=y CONFIG_I2C=y CONFIG_REGMAP=y; do
    grep -Fxq "$config" "$kernel_build/.config"
done
for config in CONFIG_IIO_BUFFER CONFIG_IIO_TRIGGER; do
    grep -Fxq "# $config is not set" "$kernel_build/.config"
done
test -s "$kernel_build/Module.symvers"
test -s "$kernel_build/include/generated/autoconf.h"
mkdir -p "$work_dir/modules"
module_dir="$work_dir/modules"
sha256sum "$kernel_build/.config" "$kernel_build/Module.symvers" > "$work_dir/kernel-inputs.sha256"

for source in \
    drivers/base/regmap/regmap-i2c.c \
    drivers/base/regmap/internal.h \
    drivers/iio/common/st_sensors/st_sensors_core.c \
    drivers/iio/common/st_sensors/st_sensors_core.h \
    drivers/iio/common/st_sensors/st_sensors_i2c.c \
    drivers/iio/accel/st_accel_core.c \
    drivers/iio/accel/st_accel_i2c.c \
    drivers/iio/accel/st_accel.h; do
    cp -- "$kernel_source/$source" "$module_dir/"
done

cat > "$module_dir/Makefile" <<'KBUILD'
obj-m += regmap-i2c.o st_sensors.o st_sensors_i2c.o st_accel.o st_accel_i2c.o
st_sensors-y := st_sensors_core.o
st_accel-y := st_accel_core.o
ccflags-y += -DCONFIG_REGMAP_I2C_MODULE=1
KBUILD

make -C "$kernel_source" O="$kernel_build" M="$module_dir" MO="$module_dir" ARCH=arm \
    CROSS_COMPILE="${CROSS_COMPILE:-arm-linux-gnueabihf-}" modules
sha256sum --check "$work_dir/kernel-inputs.sha256"

cd "$module_dir"
sha256sum regmap-i2c.ko st_sensors.ko st_sensors_i2c.ko st_accel.ko st_accel_i2c.ko > SHA256SUMS
printf 'Raw IIO modules: %s\n' "$module_dir"
