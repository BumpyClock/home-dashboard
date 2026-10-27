#!/usr/bin/env bash
set -euo pipefail

: "${PF_SYSROOT:?Set PF_SYSROOT to an Arch Linux ARM armv7h sysroot}"
PF_SYSROOT=$(realpath "$PF_SYSROOT")
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
build_dir=$(realpath -m "${PF_BUILD_DIR:-$HOME/.cache/photoframe-proto-build}")
case "$build_dir/" in "$source_dir/"*) echo 'PF_BUILD_DIR must be outside the source repository' >&2; exit 1;; esac
mkdir -p "$build_dir"

for tool in cargo rustc arm-linux-gnueabihf-gcc pkg-config; do
    command -v "$tool" >/dev/null
done
for file in usr/lib/Scrt1.o usr/lib/crti.o usr/lib/crtn.o usr/lib/pkgconfig/libinput.pc usr/lib/pkgconfig/gbm.pc; do
    test -f "$PF_SYSROOT/$file"
done

export PF_SYSROOT
cat > "$build_dir/arm-linker" <<'LINKER'
#!/usr/bin/env bash
set -euo pipefail
exec arm-linux-gnueabihf-gcc --sysroot="$PF_SYSROOT" -nostartfiles \
    "$PF_SYSROOT/usr/lib/Scrt1.o" "$PF_SYSROOT/usr/lib/crti.o" \
    "$(arm-linux-gnueabihf-gcc -print-file-name=crtbeginS.o)" \
    "$@" -L"$PF_SYSROOT/usr/lib" -Wl,-rpath-link,"$PF_SYSROOT/usr/lib" \
    "$(arm-linux-gnueabihf-gcc -print-file-name=crtendS.o)" "$PF_SYSROOT/usr/lib/crtn.o"
LINKER
chmod 0755 "$build_dir/arm-linker"

export CARGO_TARGET_DIR="$build_dir/target"
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER="$build_dir/arm-linker"
export CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
export AR_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-ar
export CFLAGS_armv7_unknown_linux_gnueabihf="--sysroot=$PF_SYSROOT"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR_armv7_unknown_linux_gnueabihf="$PF_SYSROOT"
export PKG_CONFIG_LIBDIR_armv7_unknown_linux_gnueabihf="$PF_SYSROOT/usr/lib/pkgconfig:$PF_SYSROOT/usr/share/pkgconfig"
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS='-C target-cpu=cortex-a7 -C target-feature=+neon'

cd "$source_dir"
image_aa=${PF_FEMTOVG_IMAGE_AA_FIX:-1}
quarter_turn=${PF_FEMTOVG_QUARTER_TURN_FIX:-1}
render_trace=${PF_RENDER_TRACE:-0}
texture_prewarm=${PF_TEXTURE_PREWARM:-1}
for option in "$image_aa" "$quarter_turn" "$render_trace" "$texture_prewarm"; do
    [[ $option == 0 || $option == 1 ]] || { echo 'FemtoVG fix options must be 0 or 1' >&2; exit 1; }
done
if [[ $image_aa == 1 || $quarter_turn == 1 || $render_trace == 1 || $texture_prewarm == 1 ]]; then
    command -v python3 >/dev/null
    cargo metadata --locked --offline --format-version 1 \
        --filter-platform armv7-unknown-linux-gnueabihf --no-default-features --features frame \
        > "$build_dir/femtovg-metadata.json"
    patched_source=$(python3 "$source_dir/scripts/prepare-femtovg-patch.py" \
        "$source_dir" "$build_dir/femtovg-metadata.json" "$build_dir" \
        --image-aa "$image_aa" --quarter-turn "$quarter_turn" --render-trace "$render_trace" --texture-prewarm "$texture_prewarm")
    cd "$patched_source"
    packages=()
    [[ $image_aa == 0 && $render_trace == 0 && $texture_prewarm == 0 ]] || packages+=(-p i-slint-renderer-femtovg@1.18.1)
    [[ $quarter_turn == 0 && $render_trace == 0 ]] || packages+=(-p femtovg@0.27.0)
    [[ $render_trace == 0 ]] || packages+=(-p i-slint-backend-linuxkms@1.18.1)
    cargo update --offline "${packages[@]}"
fi
cargo build --locked --release --target armv7-unknown-linux-gnueabihf --no-default-features --features frame "$@"
printf 'ARMv7 executable: %s\n' "$CARGO_TARGET_DIR/armv7-unknown-linux-gnueabihf/release/photoframe_proto"
