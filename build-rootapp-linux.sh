#!/usr/bin/env bash
#
# Builds livekit-ffi for linux-x64 natively on an Ubuntu 22.04+ (or Debian-like)
# host, with NVIDIA NVENC/NVDEC and VAAPI hardware codecs, and copies the result
# to rootapp-dist/linux-x64/native/liblivekit_ffi.so.
#
# webrtc-sys/build.rs enables the hardware paths only when it finds their headers:
#   NVIDIA: $CUDA_HOME/include/cuda.h (default /usr/local/cuda). libcuda and
#           libnvcuvid are dlopened at runtime, so only the header is needed here.
#   VAAPI:  `pkg-config --variable=includedir libva` (libva-dev). libva is dlopened.
# Without them the build still succeeds with a cargo warning and software codecs only,
# so this script refuses to continue unless both are present (override with
# ALLOW_NO_NVIDIA=1 / ALLOW_NO_VAAPI=1).
#
# Environment knobs:
#   SKIP_APT=1          do not apt-get install anything
#   CUDA_HOME=/path     CUDA toolkit root (must contain include/cuda.h)
#   PROTOC=/path        use this protoc instead of the system/downloaded one
#   PROTOC_VERSION=25.2 version downloaded when no usable protoc is found
#   LLVM_VERSION=21.1.8 clang release used for webrtc-sys (needs clang 21+)
#   LLVM_ROOT=/opt/llvm-$LLVM_VERSION
#   CC/CXX              skip the LLVM download and use these clang binaries
#   OUT_DIR=rootapp-dist

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

TARGET=x86_64-unknown-linux-gnu
RID=linux-x64
OUT_DIR="${OUT_DIR:-$ROOT/rootapp-dist}"
RUST_VERSION="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)"
PROTOC_VERSION="${PROTOC_VERSION:-25.2}"
export LLVM_VERSION="${LLVM_VERSION:-21.1.8}"
export LLVM_ROOT="${LLVM_ROOT:-/opt/llvm-$LLVM_VERSION}"

log() { printf '\n=== %s ===\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Linux ] || die "run this on Linux"
[ "$(uname -m)" = x86_64 ] || die "this script builds linux-x64 on an x86_64 host"

if [ "$(id -u)" -eq 0 ]; then sudo=""; else sudo="sudo"; fi

if [ "${SKIP_APT:-0}" != 1 ]; then
  log "installing apt packages"
  $sudo apt-get update -y
  $sudo apt-get install -y \
    build-essential pkg-config curl unzip xz-utils git ca-certificates \
    lld \
    libssl-dev \
    libglib2.0-dev \
    libx11-dev libxext-dev libxfixes-dev libxdamage-dev libxrandr-dev libxcomposite-dev \
    libgl1-mesa-dev libdrm-dev libgbm-dev \
    libasound2-dev libpulse-dev \
    libva-dev
  if ! [ -f "${CUDA_HOME:-/usr/local/cuda}/include/cuda.h" ] && ! [ -f /usr/include/cuda.h ]; then
    # Ubuntu's packaged CUDA headers; NVIDIA's own cuda-toolkit in /usr/local/cuda works too.
    $sudo apt-get install -y nvidia-cuda-dev || true
  fi
fi

log "rust $RUST_VERSION"
if ! command -v rustup >/dev/null 2>&1; then
  [ -x "$HOME/.cargo/bin/rustup" ] || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
  export PATH="$HOME/.cargo/bin:$PATH"
fi
rustup toolchain install "$RUST_VERSION" --profile minimal --target "$TARGET"
rustc --version

log "protoc"
protoc_ok() {
  local v
  v="$("$1" --version 2>/dev/null | awk '{print $2}')" || return 1
  [ -n "$v" ] || return 1
  case "$v" in
    3.*) [ "$(echo "$v" | cut -d. -f2)" -ge 15 ] ;;
    *) return 0 ;;
  esac
}
if [ -n "${PROTOC:-}" ]; then
  protoc_ok "$PROTOC" || die "PROTOC=$PROTOC is not a usable protoc (need 3.15+)"
elif command -v protoc >/dev/null 2>&1 && protoc_ok "$(command -v protoc)"; then
  PROTOC="$(command -v protoc)"
else
  dir="$HOME/.local/protoc-$PROTOC_VERSION"
  if ! [ -x "$dir/bin/protoc" ]; then
    url="https://github.com/protocolbuffers/protobuf/releases/download/v$PROTOC_VERSION/protoc-$PROTOC_VERSION-linux-x86_64.zip"
    echo "fetching $url"
    tmp="$(mktemp -d)"
    curl --fail --location --silent --show-error -o "$tmp/protoc.zip" "$url"
    mkdir -p "$dir"
    unzip -q -o "$tmp/protoc.zip" -d "$dir"
    rm -rf "$tmp"
  fi
  PROTOC="$dir/bin/protoc"
fi
export PROTOC
"$PROTOC" --version

log "clang (webrtc-sys needs 21+ for libwebrtc's hermetic libc++)"
if [ -z "${CC:-}" ] || [ -z "${CXX:-}" ]; then
  llvm_bin="$(bash .github/scripts/install-clang.sh)"
  export CC="$llvm_bin/clang" CXX="$llvm_bin/clang++"
fi
"$CXX" --version | head -n1
if [ -z "${LIBCLANG_PATH:-}" ]; then
  for d in "$(dirname "$CC")/../lib" /usr/lib/x86_64-linux-gnu /usr/lib64; do
    if ls "$d"/libclang.so* >/dev/null 2>&1; then export LIBCLANG_PATH="$(cd "$d" && pwd)"; break; fi
  done
fi
[ -n "${LIBCLANG_PATH:-}" ] || die "libclang.so not found (needed by yuv-sys bindgen); set LIBCLANG_PATH"
echo "LIBCLANG_PATH=$LIBCLANG_PATH"

log "hardware codec headers"
if [ -z "${CUDA_HOME:-}" ]; then
  if [ -f /usr/local/cuda/include/cuda.h ]; then
    CUDA_HOME=/usr/local/cuda
  elif [ -f /usr/include/cuda.h ]; then
    # Pointing CUDA_HOME at /usr would add -I/usr/include ahead of the hermetic
    # libc++ -isystem dirs and break its C header wrappers, so expose only cuda.h.
    CUDA_HOME="$ROOT/target/cuda-shim"
    mkdir -p "$CUDA_HOME/include"
    ln -sf /usr/include/cuda.h "$CUDA_HOME/include/cuda.h"
  fi
fi
if [ -n "${CUDA_HOME:-}" ] && [ -f "$CUDA_HOME/include/cuda.h" ]; then
  export CUDA_HOME
  echo "NVIDIA: cuda.h in $CUDA_HOME/include"
else
  unset CUDA_HOME
  [ "${ALLOW_NO_NVIDIA:-0}" = 1 ] || die "cuda.h not found; install nvidia-cuda-dev or the CUDA toolkit, set CUDA_HOME, or ALLOW_NO_NVIDIA=1"
  echo "NVIDIA: cuda.h not found, building without NVENC/NVDEC"
fi
if libva_inc="$(pkg-config --variable=includedir libva 2>/dev/null)" && [ -n "$libva_inc" ]; then
  echo "VAAPI: libva headers in $libva_inc"
else
  [ "${ALLOW_NO_VAAPI:-0}" = 1 ] || die "libva not found by pkg-config; install libva-dev or set ALLOW_NO_VAAPI=1"
  echo "VAAPI: libva not found, building without VAAPI"
fi

log "submodules"
git submodule update --init --recursive

log "cargo build --release -p livekit-ffi --target $TARGET"
cargo build --release -p livekit-ffi --target "$TARGET"

so="target/$TARGET/release/liblivekit_ffi.so"
[ -f "$so" ] || die "$so was not produced"
dest="$OUT_DIR/$RID/native"
mkdir -p "$dest"
cp -f "$so" "$dest/liblivekit_ffi.so"
echo "copied -> $dest/liblivekit_ffi.so"

log "post-build check"
out="$dest/liblivekit_ffi.so"
file "$out" || true
fail=0
check() {
  local label="$1" pattern="$2" required="$3"
  local n
  n="$(strings -a "$out" | grep -E -c "$pattern" || true)"
  if [ "$n" -gt 0 ]; then
    echo "  $label: present ($n matching strings)"
  elif [ "$required" = 1 ]; then
    echo "  $label: MISSING"
    fail=1
  else
    echo "  $label: not built"
  fi
}
check "NVENC/NVDEC" 'libcuda\.so\.1|libnvcuvid\.so\.1|NvEncodeAPICreateInstance' "$([ -n "${CUDA_HOME:-}" ] && echo 1 || echo 0)"
check "VAAPI" 'libva\.so\.2|libva-drm\.so\.2' "$([ "${ALLOW_NO_VAAPI:-0}" = 1 ] && echo 0 || echo 1)"
echo "  glibc floor: $(objdump -T "$out" | grep -oE 'GLIBC_[0-9.]+' | sort -V | tail -n1)"
echo "  NEEDED: $(objdump -p "$out" | awk '/NEEDED/ {print $2}' | tr '\n' ' ')"
if [ "$fail" -ne 0 ]; then
  die "hardware codec strings missing from $out; check the cargo warnings from webrtc-sys"
fi
echo "done: $out"
