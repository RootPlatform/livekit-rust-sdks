#!/usr/bin/env bash
#
# Builds livekit-ffi for macOS on a Mac (Apple silicon or Intel; either host builds both
# architectures) and installs rootapp-dist/osx-arm64/native/liblivekit_ffi.dylib and
# rootapp-dist/osx-x64/native/liblivekit_ffi.dylib, each next to a build-info.json.
# Mirrors the macOS jobs of .github/workflows/ffi-builds.yml: default features, release profile,
# MACOSX_DEPLOYMENT_TARGET 11.0 (arm64) and 10.15 (x86_64). See ROOTAPP-BUILD.md.
#
# Usage: ./build-rootapp-macos.sh [--rid osx-arm64|osx-x64|all]... [--zip] [--out DIR] [--check]
#   --rid    RIDs to build, repeated or comma-separated (default: all)
#   --zip    also write <out>/ffi-macos-<arch>.zip in the upstream release layout and
#            record its sha256 in <out>/SHA256SUMS
#   --out    output directory (default: $ROOTAPP_DIST_DIR, else rootapp-dist next to this script)
#   --check  check prerequisites and print the plan; builds and installs nothing
#
# Environment: PROTOC (protoc 3.15+), PROTOC_VERSION (downloaded to ~/.local when no usable
# protoc is found, default 25.2), LIBCLANG_PATH (default: the Xcode / command line tools one).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=build-rootapp-common.sh
. "$ROOT/build-rootapp-common.sh"
cd "$ROOT"

usage() { sed -n '3,/^$/p' "$0" | sed 's/^# \{0,1\}//'; }

triple_of() {
  case "$1" in
    osx-arm64) echo aarch64-apple-darwin ;;
    osx-x64) echo x86_64-apple-darwin ;;
  esac
}

deployment_target_of() {
  case "$1" in
    osx-arm64) echo 11.0 ;;
    osx-x64) echo 10.15 ;;
  esac
}

lipo_arch_of() {
  case "$1" in
    osx-arm64) echo arm64 ;;
    osx-x64) echo x86_64 ;;
  esac
}

RIDS=""
ZIP=0
CHECK_ONLY=0
DIST="${ROOTAPP_DIST_DIR:-$ROOT/rootapp-dist}"
while [ $# -gt 0 ]; do
  case "$1" in
    --rid)
      [ $# -ge 2 ] || rk_die "--rid needs a value"
      RIDS="$RIDS $(echo "$2" | tr ',' ' ')"
      shift 2
      ;;
    --rid=*)
      RIDS="$RIDS $(echo "${1#--rid=}" | tr ',' ' ')"
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

case " $RIDS " in
  *" all "*) RIDS="" ;;
esac
selected=""
for rid in $RIDS; do
  case "$rid" in
    osx-arm64 | osx-x64) ;;
    *) rk_die "unknown RID '$rid' (expected osx-arm64, osx-x64 or all)" ;;
  esac
  case " $selected " in
    *" $rid "*) ;;
    *) selected="${selected:+$selected }$rid" ;;
  esac
done
RIDS="${selected:-osx-arm64 osx-x64}"

[ "$(uname -s)" = Darwin ] || rk_die "run this on macOS (Linux: build-rootapp-linux.sh, Windows: build-rootapp-windows.ps1)"

rk_log "host"
rk_info "macOS $(sw_vers -productVersion 2>/dev/null || echo unknown) on $(uname -m)"
rk_info "RIDs: $RIDS"
rk_info "output: $DIST"

rk_log "Xcode command line tools"
dev_dir=""
if dev_dir="$(xcode-select -p 2>/dev/null)" && [ -n "$dev_dir" ]; then
  rk_info "developer dir: $dev_dir"
else
  dev_dir=""
  rk_missing "Xcode command line tools: run xcode-select --install (or install Xcode)"
fi
if sdk_path="$(xcrun --sdk macosx --show-sdk-path 2>/dev/null)" && [ -d "$sdk_path" ]; then
  sdk_version="$(xcrun --sdk macosx --show-sdk-version 2>/dev/null || echo unknown)"
  rk_info "macOS SDK $sdk_version: $sdk_path"
  sdk_major="${sdk_version%%.*}"
  case "$sdk_major" in
    '' | *[!0-9]*) ;;
    *)
      if [ "$sdk_major" -lt 15 ]; then
        rk_warn "macOS SDK $sdk_version is old. Upstream releases link with the newest Xcode (SDK 26.x); libwebrtc's ScreenCaptureKit/AVFoundation references may fail to link. Update Xcode or the command line tools."
      fi
      ;;
  esac
else
  rk_missing "macOS SDK (xcrun --sdk macosx --show-sdk-path failed)"
fi

if [ -n "${CC:-}${CXX:-}" ]; then
  rk_warn "ignoring CC/CXX from the environment; the build uses Apple clang like upstream"
fi
CC="$(xcrun --find clang 2>/dev/null || true)"
CXX="$(xcrun --find clang++ 2>/dev/null || true)"
AR="$(xcrun --find ar 2>/dev/null || true)"
if [ -n "$CC" ] && [ -x "$CC" ] && [ -n "$CXX" ] && [ -x "$CXX" ] && [ -n "$AR" ]; then
  export CC CXX AR
  rk_info "clang: $CC ($("$CC" --version | head -n 1))"
else
  rk_missing "Apple clang (xcrun --find clang)"
fi
# webrtc-sys runs a bare `cc --print-search-dirs` to find Apple clang's runtime libraries.
cc_path="$(command -v cc || true)"
if [ "$cc_path" != /usr/bin/cc ]; then
  rk_warn "cc resolves to '${cc_path:-nothing}'; putting /usr/bin first on PATH so the build sees Apple's cc"
  export PATH="/usr/bin:$PATH"
fi

if [ -z "${LIBCLANG_PATH:-}" ]; then
  for d in "$dev_dir/Toolchains/XcodeDefault.xctoolchain/usr/lib" "$dev_dir/usr/lib" /Library/Developer/CommandLineTools/usr/lib; do
    if [ -f "$d/libclang.dylib" ]; then
      export LIBCLANG_PATH="$d"
      break
    fi
  done
fi
if [ -n "${LIBCLANG_PATH:-}" ] && [ -f "$LIBCLANG_PATH/libclang.dylib" ]; then
  rk_info "libclang (yuv-sys bindgen): $LIBCLANG_PATH/libclang.dylib"
else
  rk_missing "libclang.dylib for yuv-sys bindgen: install the Xcode command line tools or set LIBCLANG_PATH"
fi

rk_log "tools"
rk_need_tools git curl unzip shasum file lipo otool nm codesign install_name_tool
if [ "$ZIP" = 1 ]; then rk_need_tools zip; fi
rk_info "cmake/ninja: not needed (nothing in the livekit-ffi dependency graph uses CMake)"

rk_log "rust"
rk_clear_rustflags
targets=""
for rid in $RIDS; do targets="$targets $(triple_of "$rid")"; done
# shellcheck disable=SC2086
rk_setup_rust $targets

rk_log "protoc"
case "$(uname -m)" in
  arm64) rk_setup_protoc osx-aarch_64 ;;
  *) rk_setup_protoc osx-x86_64 ;;
esac

rk_log "submodules"
rk_check_submodules

rk_finish_checks

if [ "$CHECK_ONLY" = 1 ]; then
  rk_log "plan (--check: nothing was built)"
  for rid in $RIDS; do
    rk_info "$rid: MACOSX_DEPLOYMENT_TARGET=$(deployment_target_of "$rid") cargo build --release -p livekit-ffi --target $(triple_of "$rid")"
    rk_info "  -> $DIST/$rid/native/liblivekit_ffi.dylib (install name @rpath/liblivekit_ffi.dylib, ad-hoc signed) + build-info.json"
    if [ "$ZIP" = 1 ]; then rk_info "  -> $DIST/$(rk_asset "$rid") + $DIST/SHA256SUMS"; fi
  done
  rk_info "the first build of each target downloads its libwebrtc prebuilt (a few hundred MB) from github.com/livekit/rust-sdks releases"
  exit 0
fi

mkdir -p "$DIST"
DIST="$(cd "$DIST" && pwd)"
TARGET_DIR="$(rk_target_dir)"

verify_dylib() {
  local rid="$1" f="$2" arch dt archs minos id bad probe fail=0
  arch="$(lipo_arch_of "$rid")"
  dt="$(deployment_target_of "$rid")"
  rk_log "$rid: checks"
  rk_info "file: $(file -b "$f")"

  archs="$(lipo -archs "$f" 2>/dev/null || true)"
  if [ "$archs" = "$arch" ]; then
    rk_info "architecture: $archs"
  else
    rk_info "architecture: '$archs', expected exactly '$arch'"
    fail=1
  fi

  minos="$(otool -l "$f" | awk '$1 == "cmd" { c = $2 } !done && c == "LC_BUILD_VERSION" && $1 == "minos" { print $2; done = 1 } !done && c == "LC_VERSION_MIN_MACOSX" && $1 == "version" { print $2; done = 1 }')"
  if [ "$minos" = "$dt" ]; then
    rk_info "minimum macOS: $minos"
  else
    rk_info "minimum macOS: '$minos', expected $dt"
    fail=1
  fi

  id="$(otool -D "$f" | sed -n 2p)"
  if [ "$id" = "@rpath/liblivekit_ffi.dylib" ]; then
    rk_info "install name: $id"
  else
    rk_info "install name: '$id', expected @rpath/liblivekit_ffi.dylib"
    fail=1
  fi

  bad="$(otool -L "$f" | sed 1d | awk '{ print $1 }' | grep -v -x '@rpath/liblivekit_ffi.dylib' | grep -v -E '^(/usr/lib/|/System/Library/)' || true)"
  if [ -z "$bad" ]; then
    rk_info "linked libraries: system only ($(otool -L "$f" | sed 1d | grep -c -E '/usr/lib/|/System/Library/') entries)"
  else
    rk_info "non-system libraries linked (another Mac would not have them): $(printf '%s' "$bad" | tr '\n' ' ')"
    fail=1
  fi

  if nm -gU "$f" 2>/dev/null | grep -q ' _livekit_ffi_initialize$' \
    || { command -v dyld_info >/dev/null 2>&1 && dyld_info -exports "$f" 2>/dev/null | grep -q '_livekit_ffi_initialize'; }; then
    rk_info "exports _livekit_ffi_initialize"
  else
    rk_info "_livekit_ffi_initialize is not exported"
    fail=1
  fi

  for probe in livekit_ffi_initialize VTDecompressionSession; do
    if grep -aqF -- "$probe" "$f"; then
      rk_info "capability string: $probe"
    else
      rk_info "capability string missing: $probe (download_ffi.sh would reject this build)"
      fail=1
    fi
  done

  if codesign --verify --strict "$f" 2>/dev/null; then
    rk_info "code signature: valid (ad-hoc)"
  else
    rk_info "code signature: invalid"
    fail=1
  fi

  [ "$fail" = 0 ] || rk_die "$f failed the checks above"
}

build_rid() {
  local rid="$1" triple dt src work f out
  triple="$(triple_of "$rid")"
  dt="$(deployment_target_of "$rid")"
  rk_log "$rid: cargo build --release -p livekit-ffi --target $triple (MACOSX_DEPLOYMENT_TARGET=$dt)"
  MACOSX_DEPLOYMENT_TARGET="$dt" cargo build --release -p livekit-ffi --target "$triple"

  src="$TARGET_DIR/$triple/release/liblivekit_ffi.dylib"
  [ -f "$src" ] || rk_die "$src was not produced"
  work="$TARGET_DIR/rootapp-dist-work/$rid"
  rm -rf "$work"
  mkdir -p "$work"
  f="$work/liblivekit_ffi.dylib"
  cp -f "$src" "$f"
  # rustc records the absolute build path as the install name; .NET loads the dylib by path,
  # so a relocatable @rpath id loses nothing. Any edit voids the linker signature, so re-sign.
  if ! out="$(install_name_tool -id @rpath/liblivekit_ffi.dylib "$f" 2>&1)"; then
    printf '%s\n' "$out" >&2
    rk_die "install_name_tool failed on $f"
  fi
  codesign --force --sign - "$f"

  verify_dylib "$rid" "$f"
  rk_install_lib "$f" "$DIST/$rid/native"
  rk_write_build_info "$DIST/$rid/native" "$rid" "$triple" liblivekit_ffi.dylib "macOS $dt" "VideoToolbox"
  if [ "$ZIP" = 1 ]; then
    rk_package "$rid" "$DIST/$rid/native/liblivekit_ffi.dylib"
  fi
}

for rid in $RIDS; do
  build_rid "$rid"
done

rk_log "done"
for rid in $RIDS; do
  rk_info "$DIST/$rid/native/liblivekit_ffi.dylib"
done
# shellcheck disable=SC2086
rk_handoff_hint $RIDS
