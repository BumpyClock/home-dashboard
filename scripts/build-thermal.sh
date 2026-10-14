#!/usr/bin/env bash
set -euo pipefail

: "${PF_KERNEL_BUILD:?Set PF_KERNEL_BUILD to the exact prepared frame kernel build}"
kernel_build=$(realpath "$PF_KERNEL_BUILD")
kernel_source=$(realpath "$kernel_build/source")
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
work_dir=$(realpath -m "${PF_THERMAL_BUILD_DIR:-$HOME/.cache/photoframe-thermal-build}")
case "$work_dir/" in "$source_dir/"*|"$kernel_build/"*|"$kernel_source/"*) echo 'PF_THERMAL_BUILD_DIR must be outside source and kernel input trees' >&2; exit 1;; esac
for config in CONFIG_MODULES=y CONFIG_IIO=y CONFIG_THERMAL=y CONFIG_THERMAL_OF=y CONFIG_REGMAP_MMIO=y CONFIG_REGMAP_IRQ=y; do
    grep -Fxq "$config" "$kernel_build/.config"
done
test -s "$kernel_build/Module.symvers"
mkdir -p "$work_dir/modules"
sha256sum "$kernel_build/.config" "$kernel_build/Module.symvers" > "$work_dir/kernel-inputs.sha256"
if [[ ! -d "$work_dir/kernel-build" ]]; then
    cp -a --reflink=auto "$kernel_build" "$work_dir/kernel-build"
fi
cmp "$kernel_build/.config" "$work_dir/kernel-build/.config"
cmp "$kernel_build/Module.symvers" "$work_dir/kernel-build/Module.symvers"
module_dir="$work_dir/modules"
cp "$kernel_source/drivers/iio/adc/sun4i-gpadc-iio.c" "$module_dir/"
printf 'obj-m += sun4i-gpadc-iio.o\n' > "$module_dir/Makefile"
make -C "$kernel_source" O="$work_dir/kernel-build" M="$module_dir" MO="$module_dir" \
    ARCH=arm CROSS_COMPILE="${CROSS_COMPILE:-arm-linux-gnueabihf-}" modules
sha256sum --check "$work_dir/kernel-inputs.sha256"
cd "$module_dir"
sha256sum sun4i-gpadc-iio.ko > SHA256SUMS
printf 'A33 temperature module: %s\n' "$module_dir/sun4i-gpadc-iio.ko"
