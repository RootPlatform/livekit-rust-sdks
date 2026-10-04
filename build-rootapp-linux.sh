#!/usr/bin/env bash
#
# Builds livekit-ffi for Linux natively (no Docker) and installs
# rootapp-dist/<rid>/native/liblivekit_ffi.so next to a build-info.json.
#   linux-x64    on an x86_64 host, with NVIDIA NVENC/NVDEC and VAAPI (both required by default).
#   linux-arm64  on an aarch64 host, or cross-compiled from x86_64 (setup in ROOTAPP-BUILD.md).
#                Software video like the official build; ARM64_NVIDIA=1 adds NVENC/NVDEC.
# The result needs the build host's glibc or newer: build on Ubuntu 22.04 (glibc 2.35, the
# desktop app's floor) for anything that ships. See ROOTAPP-BUILD.md.
#
# Usage: ./build-rootapp-linux.sh [--rid linux-x64|linux-arm64] [--zip] [--out DIR] [--no-apt] [--check]
#   --rid     RID to build (default: the host's)
#   --zip     also write <out>/ffi-linux-<arch>.zip in the upstream release layout and
#             record its sha256 in <out>/SHA256SUMS
#   --out     output directory (default: $ROOTAPP_DIST_DIR, else rootapp-dist next to this script)
#   --no-apt  do not apt-get install the build dependencies (same as SKIP_APT=1)
#   --check   check prerequisites and print the plan; builds and installs nothing
#
# Environment:
#   CUDA_HOME=/path    CUDA root with include/cuda.h (default /usr/local/cuda, else /usr/include/cuda.h)
#   ALLOW_NO_NVIDIA=1  linux-x64: build without NVENC/NVDEC when cuda.h is missing
#   ALLOW_CUDA13=1     accept CUDA 13+ headers (the result then needs NVIDIA driver r555+)
#   ALLOW_NO_VAAPI=1   linux-x64: build without VAAPI when libva-dev is missing
#   ARM64_NVIDIA=1     linux-arm64: also build NVENC/NVDEC (needs cuda.h)
#   ALLOW_JETSON=1     linux-arm64: allow a Jetson host (the result hard-links Jetson libraries)
#   MAX_GLIBC=2.35     fail when the result needs a newer glibc; "none" skips the check
#   CROSS_PKGCONFIG_DIR  cross builds: arm64 .pc files (default /usr/lib/aarch64-linux-gnu/pkgconfig)
#   PROTOC, PROTOC_VERSION (25.2), LLVM_VERSION (21.1.8), LLVM_ROOT (/opt/llvm-$LLVM_VERSION),
#   CC/CXX (a clang 21+; skips the LLVM download), LIBCLANG_PATH, SKIP_APT=1

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=build-rootapp-common.sh
. "$ROOT/build-rootapp-common.sh"
cd "$ROOT"

usage() { sed -n '3,/^$/p' "$0" | sed 's/^# \{0,1\}//'; }

version_gt() {
  [ -n "$1" ] && [ -n "$2" ] && [ "$1" != "$2" ] \
    && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1)" = "$1" ]
}

RID=""
ZIP=0
CHECK_ONLY=0
SKIP_APT="${SKIP_APT:-0}"
DIST="${ROOTAPP_DIST_DIR:-$ROOT/rootapp-dist}"
while [ $# -gt 0 ]; do
  case "$1" in
    --rid)
      [ $# -ge 2 ] || rk_die "--rid needs a value"
      RID="$2"
      shift 2
      ;;
    --rid=*)
      RID="${1#--rid=}"
      shift
      ;;
    --zip)
      ZIP=1
      shift
      ;;
    --out)
      [ $# -ge 2 ] || rk_die "--out needs a value"
      DIST="$2"
      shift 2
      ;;
    --out=*)
      DIST="${1#--out=}"
      shift
      ;;
    --no-apt)
      SKIP_APT=1
      shift
      ;;
    --check)
      CHECK_ONLY=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *) rk_die "unknown argument $1 (see --help)" ;;
  esac
done

[ "$(uname -s)" = Linux ] || rk_die "run this on Linux (macOS: build-rootapp-macos.sh, Windows: build-rootapp-windows.ps1)"
case "$(uname -m)" in
  x86_64 | amd64)
    HOST_RID=linux-x64
    PROTOC_PLATFORM=linux-x86_64
    ;;
  aarch64 | arm64)
    HOST_RID=linux-arm64
    PROTOC_PLATFORM=linux-aarch_64
    ;;
  *) rk_die "unsupported host architecture $(uname -m)" ;;
esac
RID="${RID:-$HOST_RID}"
case "$RID" in
  linux-x64)
    TARGET=x86_64-unknown-linux-gnu
    ELF_MACHINE="X86-64"
    ;;
  linux-arm64)
    TARGET=aarch64-unknown-linux-gnu
    ELF_MACHINE="AArch64"
    ;;
  *) rk_die "unknown RID '$RID' (expected linux-x64 or linux-arm64)" ;;
esac
CROSS=0
if [ "$RID" != "$HOST_RID" ]; then
  [ "$HOST_RID" = linux-x64 ] && [ "$RID" = linux-arm64 ] \
    || rk_die "linux-x64 has to be built on an x86_64 host; only linux-arm64 can be cross-compiled (from x86_64)"
  CROSS=1
fi
MAX_GLIBC="${MAX_GLIBC:-2.35}"
export LLVM_VERSION="${LLVM_VERSION:-21.1.8}"
export LLVM_ROOT="${LLVM_ROOT:-/opt/llvm-$LLVM_VERSION}"
TARGET_DIR="$(rk_target_dir)"

rk_log "host"
# shellcheck disable=SC2016
distro="$(sh -c '. /etc/os-release && echo "$PRETTY_NAME"' 2>/dev/null || echo "unknown distro")"
host_glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{ print $2 }' || true)"
rk_info "$distro, $(uname -m), glibc ${host_glibc:-unknown}"
if [ "$CROSS" = 1 ]; then
  rk_info "RID: $RID ($TARGET), cross-compiled from x86_64"
else
  rk_info "RID: $RID ($TARGET)"
fi
rk_info "output: $DIST"
if [ "$MAX_GLIBC" != none ] && version_gt "$host_glibc" "$MAX_GLIBC"; then
  rk_warn "this host has glibc $host_glibc, so the result will probably need more than MAX_GLIBC=$MAX_GLIBC and fail the floor check. Build on Ubuntu 22.04 for anything that ships, or set MAX_GLIBC=none for a build that only runs on this machine."
fi

if [ "$(id -u)" -eq 0 ]; then
  SUDO=""
elif command -v sudo >/dev/null 2>&1; then
  SUDO="sudo"
else
  SUDO="none"
fi

APT_PACKAGES="build-essential pkg-config curl unzip zip xz-utils git ca-certificates file binutils lld
  libssl-dev libglib2.0-dev libx11-dev libxext-dev libxfixes-dev libxdamage-dev libxrandr-dev libxcomposite-dev
  libgl1-mesa-dev libdrm-dev libgbm-dev libasound2-dev libpulse-dev"
if [ "$RID" = linux-x64 ]; then APT_PACKAGES="$APT_PACKAGES libva-dev"; fi
if [ "$CROSS" = 1 ]; then APT_PACKAGES="$APT_PACKAGES gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross"; fi
# shellcheck disable=SC2086
APT_PACKAGES="$(echo $APT_PACKAGES)"

rk_log "build dependencies"
APT_PENDING=0
if [ "$SKIP_APT" = 1 ]; then
  rk_info "SKIP_APT=1: not installing anything; needs the equivalents of: $APT_PACKAGES"
elif ! command -v apt-get >/dev/null 2>&1; then
  rk_warn "no apt-get here; install the equivalents of these Debian/Ubuntu packages yourself: $APT_PACKAGES"
elif [ "$CHECK_ONLY" = 1 ]; then
  missing_pkgs=""
  for p in $APT_PACKAGES; do
    # shellcheck disable=SC2016
    dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'install ok installed' || missing_pkgs="$missing_pkgs $p"
  done
  if [ -n "$missing_pkgs" ]; then
    APT_PENDING=1
    rk_info "the build apt-installs:$missing_pkgs"
  else
    rk_info "all apt packages installed"
  fi
else
  [ "$SUDO" != none ] || rk_die "apt-get needs root or sudo; install the packages yourself and rerun with SKIP_APT=1: $APT_PACKAGES"
  # shellcheck disable=SC2086
  $SUDO apt-get update -y
  # shellcheck disable=SC2086
  $SUDO apt-get install -y --no-install-recommends $APT_PACKAGES
fi

need_tool() {
  if command -v "$1" >/dev/null 2>&1; then
    rk_info "$1: $(command -v "$1")"
  elif [ "$APT_PENDING" = 1 ]; then
    rk_info "$1: not installed yet (apt package $2)"
  else
    rk_missing "$1 (Debian/Ubuntu package $2)"
  fi
}

rk_log "tools"
need_tool cc build-essential
need_tool pkg-config pkg-config
need_tool git git
need_tool curl curl
need_tool unzip unzip
need_tool xz xz-utils
need_tool file file
need_tool readelf binutils
need_tool sha256sum coreutils
if [ "$ZIP" = 1 ]; then need_tool zip zip; fi
if [ "$RID" = linux-arm64 ]; then need_tool ld.lld lld; fi
rk_info "cmake/ninja: not needed (nothing in the livekit-ffi dependency graph uses CMake)"

if [ "$CROSS" = 1 ]; then
  rk_log "cross toolchain (x86_64 -> aarch64)"
  need_tool aarch64-linux-gnu-gcc gcc-aarch64-linux-gnu
  need_tool aarch64-linux-gnu-g++ g++-aarch64-linux-gnu
  if [ "$APT_PENDING" != 1 ] && command -v aarch64-linux-gnu-gcc >/dev/null 2>&1 &&
    [ "$(aarch64-linux-gnu-gcc -print-file-name=libstdc++.so)" = libstdc++.so ]; then
    rk_missing "arm64 libstdc++.so for cxx and link-cplusplus (Debian/Ubuntu package g++-aarch64-linux-gnu)"
  fi
  CROSS_PC_DIR="${CROSS_PKGCONFIG_DIR:-/usr/lib/aarch64-linux-gnu/pkgconfig}"
  for pc in glib-2.0 gobject-2.0 gio-2.0; do
    if [ -f "$CROSS_PC_DIR/$pc.pc" ]; then
      rk_info "arm64 $pc.pc: $CROSS_PC_DIR/$pc.pc"
    else
      rk_missing "$CROSS_PC_DIR/$pc.pc: install libglib2.0-dev:arm64 (needs dpkg --add-architecture arm64 and arm64 apt sources; see ROOTAPP-BUILD.md)"
    fi
  done
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
  export PKG_CONFIG_ALLOW_CROSS=1
  export PKG_CONFIG_LIBDIR="$CROSS_PC_DIR:/usr/share/pkgconfig"
  export PKG_CONFIG_PATH=""
  rk_info "linker: aarch64-linux-gnu-gcc (-fuse-ld=lld from .cargo/config.toml); pkg-config: $PKG_CONFIG_LIBDIR"
fi

if [ "$APT_PENDING" != 1 ]; then
  if pkg-config --exists glib-2.0 gobject-2.0 gio-2.0 2>/dev/null; then
    rk_info "glib/gobject/gio headers: $(pkg-config --modversion glib-2.0)"
  elif [ "$CROSS" != 1 ]; then
    rk_missing "glib-2.0/gobject-2.0/gio-2.0 via pkg-config (libglib2.0-dev)"
  fi
fi

rk_log "rust"
rk_clear_rustflags
rk_setup_rust "$TARGET"

rk_log "protoc"
rk_setup_protoc "$PROTOC_PLATFORM"

rk_log "clang 21+ (libwebrtc.a is built against Chromium's hermetic libc++)"
if [ -n "${CC:-}" ] && [ -n "${CXX:-}" ]; then
  rk_info "using CC=$CC CXX=$CXX from the environment"
elif [ -x "$LLVM_ROOT/bin/clang++" ]; then
  export CC="$LLVM_ROOT/bin/clang" CXX="$LLVM_ROOT/bin/clang++"
elif [ "$CHECK_ONLY" = 1 ]; then
  rk_info "clang: the build installs LLVM $LLVM_VERSION to $LLVM_ROOT with .github/scripts/install-clang.sh (sudo)"
else
  [ "$SUDO" != none ] || rk_die "installing LLVM to $LLVM_ROOT needs root or sudo; set CC/CXX to a clang 21+ instead"
  llvm_bin="$(bash .github/scripts/install-clang.sh)"
  export CC="$llvm_bin/clang" CXX="$llvm_bin/clang++"
fi
if [ -n "${CXX:-}" ] && command -v "$CXX" >/dev/null 2>&1; then
  clang_major="$("$CXX" -dM -E -x c++ /dev/null 2>/dev/null | awk '$2 == "__clang_major__" { print $3 }')"
  if [ -z "$clang_major" ]; then
    rk_missing "$CXX is not clang; webrtc-sys needs clang 21+ on Linux"
  elif [ "$clang_major" -lt 21 ]; then
    rk_missing "$CXX is clang $clang_major; webrtc-sys needs clang 21+ (unset CC/CXX to use LLVM $LLVM_VERSION)"
  else
    rk_info "clang: $CXX ($("$CXX" --version | head -n 1))"
  fi
  if [ -z "${LIBCLANG_PATH:-}" ]; then
    cxx_dir="$(dirname "$(readlink -f "$(command -v "$CXX")")")"
    for d in "$cxx_dir/../lib" ${clang_major:+"/usr/lib/llvm-$clang_major/lib"} /usr/lib/x86_64-linux-gnu /usr/lib/aarch64-linux-gnu /usr/lib64; do
      if ls "$d"/libclang.so* >/dev/null 2>&1; then
        LIBCLANG_PATH="$(cd "$d" && pwd)"
        export LIBCLANG_PATH
        break
      fi
    done
  fi
  if [ -n "${LIBCLANG_PATH:-}" ]; then
    rk_info "libclang (yuv-sys bindgen): $LIBCLANG_PATH"
  else
    rk_missing "libclang.so for yuv-sys bindgen; set LIBCLANG_PATH"
  fi
elif [ "$CHECK_ONLY" != 1 ]; then
  rk_missing "clang++ (CXX=${CXX:-unset})"
fi

rk_log "hardware video codecs"
NVIDIA=0
VAAPI=0
NO_CUDA_DIR="$TARGET_DIR/rootapp-no-cuda"
want_nvidia=0
require_nvidia=0
if [ "$RID" = linux-x64 ]; then
  want_nvidia=1
  [ "${ALLOW_NO_NVIDIA:-0}" = 1 ] || require_nvidia=1
elif [ "${ARM64_NVIDIA:-0}" = 1 ]; then
  want_nvidia=1
  require_nvidia=1
fi
if [ "$want_nvidia" = 1 ]; then
  case "${CUDA_HOME:-}" in
    /usr | /usr/) CUDA_HOME="" ;;
  esac
  if [ -z "${CUDA_HOME:-}" ]; then
    if [ -f /usr/local/cuda/include/cuda.h ]; then
      CUDA_HOME=/usr/local/cuda
    elif [ -f /usr/include/cuda.h ]; then
      # CUDA_HOME=/usr would put -I/usr/include ahead of the hermetic libc++ and break its
      # C header wrappers, so expose only cuda.h.
      CUDA_HOME="$TARGET_DIR/rootapp-cuda-shim"
      if [ "$CHECK_ONLY" != 1 ]; then
        mkdir -p "$CUDA_HOME/include"
        ln -sf /usr/include/cuda.h "$CUDA_HOME/include/cuda.h"
      fi
    fi
  fi
  if [ -n "${CUDA_HOME:-}" ] && { [ -f "$CUDA_HOME/include/cuda.h" ] || [ "$CUDA_HOME" = "$TARGET_DIR/rootapp-cuda-shim" ]; }; then
    NVIDIA=1
    export CUDA_HOME
    rk_info "NVENC/NVDEC: on (cuda.h via $CUDA_HOME/include; libcuda, libnvcuvid and libnvidia-encode are dlopened at runtime)"
    cuda_version=$(sed -n 's/^#define[[:space:]]\{1,\}CUDA_VERSION[[:space:]]\{1,\}\([0-9]\{1,\}\).*/\1/p' "$CUDA_HOME/include/cuda.h" | head -n 1)
    if [ -n "$cuda_version" ] && [ "$cuda_version" -ge 13000 ] && [ "${ALLOW_CUDA13:-0}" != 1 ]; then
      rk_missing "cuda.h is from CUDA $((cuda_version / 1000)).$((cuda_version % 1000 / 10)); a library built against CUDA 13+ headers aborts on NVIDIA drivers older than r555. Point CUDA_HOME at CUDA 12.x headers, or set ALLOW_CUDA13=1 if every target machine runs r555 or newer"
    fi
  elif [ "$require_nvidia" = 1 ]; then
    rk_missing "cuda.h for NVENC/NVDEC: sudo apt-get install --no-install-recommends nvidia-cuda-dev (Ubuntu multiverse) or NVIDIA's CUDA toolkit, or set CUDA_HOME. ALLOW_NO_NVIDIA=1 builds without it, but download_ffi.sh rejects a linux-x64 build that lacks NVIDIA"
  else
    rk_info "NVENC/NVDEC: off (no cuda.h)"
  fi
else
  rk_info "NVENC/NVDEC: off, like the official linux-arm64 build (ARM64_NVIDIA=1 builds it)"
fi
if [ "$NVIDIA" = 0 ]; then
  # webrtc-sys falls back to /usr/local/cuda when CUDA_HOME is unset; point it at an empty path.
  CUDA_HOME="$NO_CUDA_DIR"
  export CUDA_HOME
fi

if [ "$RID" = linux-x64 ]; then
  libva_inc="$(pkg-config --variable=includedir libva 2>/dev/null || true)"
  if [ -n "$libva_inc" ] && [ -f "$libva_inc/va/va.h" ]; then
    VAAPI=1
    rk_info "VAAPI: on (libva headers in $libva_inc; libva and libva-drm are dlopened at runtime)"
  elif [ "$APT_PENDING" = 1 ]; then
    VAAPI=1
    rk_info "VAAPI: on once apt installs libva-dev"
  elif [ "${ALLOW_NO_VAAPI:-0}" = 1 ]; then
    rk_info "VAAPI: off (ALLOW_NO_VAAPI=1)"
  else
    rk_missing "libva headers for VAAPI (libva-dev). ALLOW_NO_VAAPI=1 builds without it, but download_ffi.sh rejects a linux-x64 build that lacks VAAPI"
  fi
else
  rk_info "VAAPI: not built for arm64 (webrtc-sys only builds it for x86_64, like upstream)"
fi

if [ "$RID" = linux-arm64 ] && [ "$CROSS" = 0 ] && [ -d /usr/src/jetson_multimedia_api/include ]; then
  if [ "${ALLOW_JETSON:-0}" = 1 ]; then
    rk_warn "Jetson host: webrtc-sys links libnvv4l2/libnvbufsurface/libv4l2 directly, so this .so only loads on Jetson"
  else
    rk_missing "this is a Jetson (/usr/src/jetson_multimedia_api): webrtc-sys would link libnvv4l2/libnvbufsurface/libv4l2 directly and the .so would not load on other arm64 machines. Build on another arm64 host, or set ALLOW_JETSON=1 for a Jetson-only build"
  fi
fi

HW_DESC=""
[ "$NVIDIA" = 0 ] || HW_DESC="NVENC/NVDEC"
[ "$VAAPI" = 0 ] || HW_DESC="${HW_DESC:+$HW_DESC, }VAAPI"
HW_DESC="${HW_DESC:-none (software codecs)}"

rk_log "submodules"
rk_check_submodules

rk_finish_checks

if [ "$CHECK_ONLY" = 1 ]; then
  rk_log "plan (--check: nothing was built or installed)"
  rk_info "cargo build --release -p livekit-ffi --target $TARGET"
  rk_info "  -> $DIST/$RID/native/liblivekit_ffi.so + build-info.json"
  if [ "$ZIP" = 1 ]; then rk_info "  -> $DIST/$(rk_asset "$RID") + $DIST/SHA256SUMS"; fi
  rk_info "hardware video: $HW_DESC"
  rk_info "glibc: the result needs this host's glibc (${host_glibc:-unknown}) at most; checked against MAX_GLIBC=$MAX_GLIBC"
  rk_info "the first build downloads the libwebrtc prebuilt (a few hundred MB) from github.com/livekit/rust-sdks releases"
  exit 0
fi

mkdir -p "$DIST"
DIST="$(cd "$DIST" && pwd)"

rk_log "cargo build --release -p livekit-ffi --target $TARGET"
# webrtc-sys/build.rs detects CUDA and libva but does not rerun when they change, so a
# cached build would keep the old codec set. Clean it whenever the configuration differs.
hw_config="rid=$RID nvidia=$NVIDIA vaapi=$VAAPI cuda_home=$CUDA_HOME cxx=$CXX"
stamp="$TARGET_DIR/$TARGET/release/.rootapp-hw-config"
if ls -d "$TARGET_DIR/$TARGET/release/build/webrtc-sys-"* >/dev/null 2>&1; then
  previous="$(cat "$stamp" 2>/dev/null || echo unknown)"
  if [ "$previous" != "$hw_config" ]; then
    rk_info "codec configuration changed ($previous -> $hw_config); cleaning webrtc-sys"
    cargo clean --release --target "$TARGET" -p webrtc-sys
  fi
fi
# yuv-sys's bindgen loads libclang from LIBCLANG_PATH, which misses the LLVM tarball's builtin
# headers (stddef.h) unless the compiler's resource directory is passed explicitly.
if [ -z "${BINDGEN_EXTRA_CLANG_ARGS:-}" ] && resource_dir="$("$CC" -print-resource-dir 2>/dev/null)" && [ -d "$resource_dir/include" ]; then
  export BINDGEN_EXTRA_CLANG_ARGS="-resource-dir=$resource_dir"
  rk_info "bindgen: -resource-dir=$resource_dir"
fi
cargo build --release -p livekit-ffi --target "$TARGET"
printf '%s\n' "$hw_config" > "$stamp"

so="$TARGET_DIR/$TARGET/release/liblivekit_ffi.so"
[ -f "$so" ] || rk_die "$so was not produced"

rk_log "checks"
fail=0
check_probe() {
  if grep -aqF -- "$1" "$so"; then
    rk_info "capability string: $1"
  else
    rk_info "capability string missing: $1"
    fail=1
  fi
}

rk_info "file: $(file -b "$so")"
machine="$(readelf -h "$so" | awk -F: '$1 ~ /Machine/ { sub(/^[ \t]+/, "", $2); print $2 }')"
case "$machine" in
  *"$ELF_MACHINE"*) rk_info "machine: $machine" ;;
  *)
    rk_info "machine: '$machine', expected $ELF_MACHINE"
    fail=1
    ;;
esac

if readelf -W --dyn-syms "$so" | awk '$7 != "UND" && $8 ~ /^livekit_ffi_initialize(@|$)/ { found = 1 } END { exit !found }'; then
  rk_info "exports livekit_ffi_initialize"
else
  rk_info "livekit_ffi_initialize is not exported"
  fail=1
fi

needed="$(readelf -d -W "$so" | awk '/\(NEEDED\)/ { n = $NF; gsub(/[][]/, "", n); print n }' | tr '\n' ' ')"
rk_info "NEEDED: $needed"
allowed="libc.so.6 libm.so.6 libdl.so.2 libpthread.so.0 librt.so.1 libgcc_s.so.1 ld-linux-x86-64.so.2 ld-linux-aarch64.so.1"
unexpected=""
for n in $needed; do
  case " $allowed " in
    *" $n "*) ;;
    *) unexpected="$unexpected $n" ;;
  esac
done
if [ -n "$unexpected" ]; then
  if [ "${ALLOW_JETSON:-0}" = 1 ]; then
    rk_info "extra NEEDED libraries (Jetson build):$unexpected"
  else
    rk_info "unexpected NEEDED libraries:$unexpected (GPU, X11 and audio libraries must be dlopened, not linked)"
    fail=1
  fi
fi

undefined_gpu="$(readelf -W --dyn-syms "$so" | awk '$7 == "UND" { print $8 }' | grep -E '^(cu[A-Z]|cuvid[A-Z]|va[A-Z]|Nv|X[A-Z]|xcb_|drm[A-Z]|gbm_)' | tr '\n' ' ' || true)"
if [ -n "$undefined_gpu" ]; then
  rk_info "undefined GPU/X11 symbols, missing from the lazy-load trampolines (they would fail at runtime): $undefined_gpu"
  fail=1
fi

GLIBC_FLOOR="$(readelf -W -V "$so" | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' | sed 's/^GLIBC_//' | sort -Vu | tail -n 1 || true)"
rk_info "needs glibc ${GLIBC_FLOOR:-(unknown)} or newer"
if [ -z "$GLIBC_FLOOR" ]; then
  fail=1
elif [ "$MAX_GLIBC" != none ] && version_gt "$GLIBC_FLOOR" "$MAX_GLIBC"; then
  rk_info "glibc $GLIBC_FLOOR is above MAX_GLIBC=$MAX_GLIBC (the desktop's GLIBC_FLOOR); build on an older distro, or MAX_GLIBC=none for a local-only build"
  fail=1
fi

check_probe livekit_ffi_initialize
check_probe "Invalid mix of IDR and non-IDR slices"
if [ "$NVIDIA" = 1 ]; then
  for p in "NVIDIA H264 Decoder" "NVIDIA H264 Encoder" libcuda.so.1 libnvcuvid.so.1 libnvidia-encode.so.1; do check_probe "$p"; done
fi
if [ "$VAAPI" = 1 ]; then
  for p in "VAAPI H264 Encoder" libva.so.2 libva-drm.so.2; do check_probe "$p"; done
fi
if [ "$RID" = linux-x64 ]; then
  missing_hw=""
  [ "$NVIDIA" = 1 ] || missing_hw="NVIDIA"
  [ "$VAAPI" = 1 ] || missing_hw="${missing_hw:+$missing_hw and }VAAPI"
  if [ -n "$missing_hw" ]; then
    rk_warn "built without $missing_hw: the desktop's download_ffi.sh requires 'NVIDIA H264 Decoder' and 'VAAPI H264 Encoder' in a linux-x64 build and will refuse this one"
  fi
fi

[ "$fail" = 0 ] || rk_die "$so failed the checks above; check the cargo warnings from webrtc-sys"

dest="$DIST/$RID/native"
rk_install_lib "$so" "$dest"
rk_write_build_info "$dest" "$RID" "$TARGET" liblivekit_ffi.so "glibc $GLIBC_FLOOR" "$HW_DESC"
if [ "$ZIP" = 1 ]; then
  rk_package "$RID" "$dest/liblivekit_ffi.so"
fi

rk_log "done"
rk_info "$dest/liblivekit_ffi.so (needs glibc $GLIBC_FLOOR+, hardware video: $HW_DESC)"
rk_handoff_hint "$RID"
