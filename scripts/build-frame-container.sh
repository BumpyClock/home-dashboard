#!/usr/bin/env bash
# Cross-build the frame binary from macOS in a Debian container (Docker).
# Creates/reuses the pf-build container, builds an Arch Linux ARM armv7h
# sysroot, then runs scripts/build-frame.sh inside it. Output lands in
# PF_OUT_DIR (default ~/.cache/pf-build-out). See docs/serial-deploy.md.
set -euo pipefail

CONTAINER=pf-build
MIRROR=https://fl.us.mirror.archlinuxarm.org/armv7h
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
out_dir=${PF_OUT_DIR:-$HOME/.cache/pf-build-out}
mkdir -p "$out_dir"
out_dir=$(cd -- "$out_dir" && pwd)

# Must match the frame's installed packages (pacman -Q over serial);
# epoch is stripped. Each package is checked against the SHA256 in the repo
# database (scripts/resolve-alarm-packages.py), fetched over HTTPS.
# Last verified against the frame: 2026-10-14 (glibc 2.43, mesa 1:26.2.4).
PKGS=(
  "glibc 2.43+r22+g8362e8ce10b2-2"
  "gcc-libs 16.1.1+r12+g301eb08fa2c5-1"
  "mesa 26.2.4-1"
  "libdrm 2.4.134-1"
  "libglvnd 1.7.0-3"
  "libinput 1.32.0-1"
  "systemd-libs 262-1"
  "libevdev 1.14.0-1"
  "mtdev 1.1.7-1"
  "fontconfig 2.18.3-2"
  "freetype2 2.14.3-1"
  "expat 2.9.0-1"
  "libpng 1.6.59-1"
  "brotli 1.2.0-1"
  "bzip2 1.0.8-6"
  "zlib 1.3.2-3"
  "libxkbcommon 1.13.2-1"
)

command -v docker >/dev/null
if ! docker inspect "$CONTAINER" >/dev/null 2>&1; then
    docker run -d --name "$CONTAINER" --platform linux/amd64 \
        -v "$source_dir:/src:ro" -v "$out_dir:/out" \
        debian:bookworm sleep infinity
fi
exec_in() { docker exec "$CONTAINER" bash -lc "$1"; }

exec_in "which cc >/dev/null 2>&1 || { apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    gcc-arm-linux-gnueabihf pkg-config python3 patch gcc \
    curl ca-certificates xz-utils zstd binutils file >/dev/null; }"
exec_in "curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null 2>&1 || true"
exec_in "source ~/.cargo/env && rustup target add armv7-unknown-linux-gnueabihf"
exec_in "source ~/.cargo/env && cd /src && cargo fetch --locked --target armv7-unknown-linux-gnueabihf"

printf '%s\n' "${PKGS[@]}" > /tmp/pf-pins.txt
docker cp /tmp/pf-pins.txt "$CONTAINER":/tmp/pf-pins.txt >/dev/null

cat > /tmp/pf-inner.sh <<INNER
set -euo pipefail
MIRROR='$MIRROR'
mkdir -p /sysroot /sysrootpkgs
python3 /src/scripts/resolve-alarm-packages.py "\$MIRROR" < /tmp/pf-pins.txt > /tmp/pf-resolved.txt
while IFS=\$'\t' read -r repo fname sha; do
    echo "  \$repo/\$fname"
    url_name=\$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "\$fname")
    curl -sS --proto '=https' --retry 5 --retry-delay 2 -m 300 --fail -o "/sysrootpkgs/\$fname" "\$MIRROR/\$repo/\$url_name"
    echo "\$sha  /sysrootpkgs/\$fname" | sha256sum -c --quiet - || { echo "CHECKSUM MISMATCH: \$fname" >&2; exit 1; }
    tar -C /sysroot -xf "/sysrootpkgs/\$fname"
done < /tmp/pf-resolved.txt
for f in usr/lib/Scrt1.o usr/lib/crti.o usr/lib/crtn.o usr/lib/pkgconfig/libinput.pc usr/lib/pkgconfig/gbm.pc; do
    test -f "/sysroot/\$f" || { echo "MISSING SYSROOT FILE: \$f" >&2; exit 1; }
done
echo "sysroot glibc max symbol: \$(arm-linux-gnueabihf-readelf --version-info /sysroot/usr/lib/libc.so.6 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
source ~/.cargo/env
export PF_SYSROOT=/sysroot PF_BUILD_DIR=/work/build
bash /src/scripts/build-frame.sh 2>&1 | tail -25
sha256sum "\$PF_BUILD_DIR/target/armv7-unknown-linux-gnueabihf/release/photoframe_proto"
cp "\$PF_BUILD_DIR/target/armv7-unknown-linux-gnueabihf/release/photoframe_proto" /out/photoframe_proto
INNER

docker cp /tmp/pf-inner.sh "$CONTAINER":/tmp/pf-inner.sh >/dev/null
docker exec "$CONTAINER" bash /tmp/pf-inner.sh
printf 'Host copy: %s/photoframe_proto\n' "$out_dir"
