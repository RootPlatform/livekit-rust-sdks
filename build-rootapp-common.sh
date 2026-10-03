# Shared helpers for build-rootapp-macos.sh and build-rootapp-linux.sh. Sourced, not run.
# Callers set ROOT (repo root), DIST (output directory) and CHECK_ONLY (1 = report, change nothing).
# Keep this bash 3.2 compatible: macOS runs scripts with /bin/bash 3.2.

RK_MISSING=0

rk_log() { printf '\n=== %s ===\n' "$*"; }

rk_info() { printf '  %s\n' "$*"; }

rk_warn() {
  printf 'warning: %s\n' "$*" >&2
  if [ "${GITHUB_ACTIONS:-}" = true ]; then printf '::warning title=RootApp FFI::%s\n' "$*"; fi
}

rk_die() {
  printf 'error: %s\n' "$*" >&2
  if [ "${GITHUB_ACTIONS:-}" = true ]; then printf '::error title=RootApp FFI::%s\n' "$*"; fi
  exit 1
}

rk_missing() {
  printf '  MISSING: %s\n' "$*" >&2
  RK_MISSING=1
}

rk_need_tools() {
  local t
  for t in "$@"; do
    if command -v "$t" >/dev/null 2>&1; then
      rk_info "$t: $(command -v "$t")"
    else
      rk_missing "$t is not on PATH"
    fi
  done
}

rk_finish_checks() {
  if [ "$RK_MISSING" != 0 ]; then
    rk_die "prerequisites are missing (see MISSING lines above and ROOTAPP-BUILD.md)"
  fi
}

rk_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

rk_asset() {
  case "$1" in
    win-x64) echo ffi-windows-x86_64.zip ;;
    win-arm64) echo ffi-windows-arm64.zip ;;
    osx-x64) echo ffi-macos-x86_64.zip ;;
    osx-arm64) echo ffi-macos-arm64.zip ;;
    linux-x64) echo ffi-linux-x86_64.zip ;;
    linux-arm64) echo ffi-linux-arm64.zip ;;
    *) return 1 ;;
  esac
}

rk_json() {
  printf '%s' "$1" | tr -d '"\\\r\n'
}

rk_rust_version() {
  sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml"
}

rk_ffi_version() {
  sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/livekit-ffi/Cargo.toml" | head -n 1
}

rk_target_dir() {
  local d="${CARGO_TARGET_DIR:-$ROOT/target}"
  case "$d" in
    /*) printf '%s\n' "$d" ;;
    *) printf '%s\n' "$ROOT/$d" ;;
  esac
}

rk_clear_rustflags() {
  local v
  for v in RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS; do
    if [ -n "${!v:-}" ]; then
      rk_warn "ignoring $v='${!v}': it would replace the per-target rustflags in .cargo/config.toml (-ObjC on macOS, -fuse-ld=lld on aarch64 Linux)"
      unset "$v"
    fi
  done
}

rk_setup_rust() {
  local t
  RK_RUST_VERSION="$(rk_rust_version)"
  [ -n "$RK_RUST_VERSION" ] || rk_die "cannot read the toolchain channel from rust-toolchain.toml"
  if ! command -v rustup >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/rustup" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
  fi
  if ! command -v rustup >/dev/null 2>&1; then
    if [ "$CHECK_ONLY" = 1 ]; then
      rk_missing "rustup (the build installs it to ~/.cargo with https://sh.rustup.rs)"
      return 0
    fi
    rk_info "installing rustup to ~/.cargo"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none --no-modify-path
    export PATH="$HOME/.cargo/bin:$PATH"
  fi
  rk_info "rustup: $(command -v rustup)"
  if [ "$CHECK_ONLY" = 1 ]; then
    if rustup toolchain list 2>/dev/null | grep -q "^$RK_RUST_VERSION"; then
      rk_info "toolchain $RK_RUST_VERSION: installed"
      for t in "$@"; do
        if rustup target list --toolchain "$RK_RUST_VERSION" --installed 2>/dev/null | grep -qx "$t"; then
          rk_info "rust target $t: installed"
        else
          rk_info "rust target $t: not installed (the build adds it)"
        fi
      done
    else
      rk_info "toolchain $RK_RUST_VERSION: not installed (the build installs it with targets: $*)"
    fi
    return 0
  fi
  rustup toolchain install "$RK_RUST_VERSION" --profile minimal --no-self-update
  for t in "$@"; do
    rustup target add --toolchain "$RK_RUST_VERSION" "$t"
  done
  rk_info "rustc: $(cd "$ROOT" && rustc --version)"
}

rk_protoc_ok() {
  local v
  v="$("$1" --version 2>/dev/null | awk '{ print $2 }')" || return 1
  case "$v" in
    "") return 1 ;;
    3.*) [ "$(echo "$v" | cut -d. -f2)" -ge 15 ] ;;
    *) return 0 ;;
  esac
}

rk_setup_protoc() {
  local platform="$1" dir url tmp
  PROTOC_VERSION="${PROTOC_VERSION:-25.2}"
  if [ -n "${PROTOC:-}" ]; then
    rk_protoc_ok "$PROTOC" || rk_die "PROTOC=$PROTOC is not a usable protoc (need 3.15 or newer)"
  elif command -v protoc >/dev/null 2>&1 && rk_protoc_ok "$(command -v protoc)"; then
    PROTOC="$(command -v protoc)"
  else
    if command -v protoc >/dev/null 2>&1; then
      rk_info "protoc on PATH is $(protoc --version 2>/dev/null); prost-build needs 3.15 or newer"
    fi
    dir="$HOME/.local/protoc-$PROTOC_VERSION"
    url="https://github.com/protocolbuffers/protobuf/releases/download/v$PROTOC_VERSION/protoc-$PROTOC_VERSION-$platform.zip"
    if ! [ -x "$dir/bin/protoc" ]; then
      if [ "$CHECK_ONLY" = 1 ]; then
        rk_info "protoc: none usable; the build downloads $url to $dir"
        return 0
      fi
      rk_info "fetching $url"
      tmp="$(mktemp -d)"
      curl --fail --location --silent --show-error -o "$tmp/protoc.zip" "$url"
      mkdir -p "$dir"
      unzip -q -o "$tmp/protoc.zip" -d "$dir"
      rm -rf "$tmp"
    fi
    PROTOC="$dir/bin/protoc"
  fi
  export PROTOC
  rk_info "protoc: $PROTOC ($("$PROTOC" --version))"
}

rk_check_submodules() {
  if [ "$CHECK_ONLY" != 1 ] && [ -e "$ROOT/.git" ] && command -v git >/dev/null 2>&1; then
    git -C "$ROOT" submodule update --init --recursive
  fi
  if [ -f "$ROOT/yuv-sys/libyuv/include/libyuv.h" ] && [ -d "$ROOT/livekit-protocol/protocol/protobufs" ]; then
    rk_info "submodules: yuv-sys/libyuv and livekit-protocol/protocol present"
  elif [ "$CHECK_ONLY" = 1 ]; then
    rk_info "submodules: not initialised (the build runs git submodule update --init --recursive)"
  else
    rk_die "submodules yuv-sys/libyuv and livekit-protocol/protocol are missing; run git submodule update --init --recursive"
  fi
}

rk_git_field() {
  case "$1" in
    commit) git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown ;;
    branch) git -C "$ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown ;;
    dirty)
      if ! git -C "$ROOT" rev-parse HEAD >/dev/null 2>&1; then
        echo false
      elif [ -n "$(git -C "$ROOT" status --porcelain --untracked-files=no 2>/dev/null)" ]; then
        echo true
      else
        echo false
      fi
      ;;
  esac
}

rk_write_build_info() {
  local dest="$1" rid="$2" target="$3" lib="$4" min_os="$5" hw="$6" f
  f="$dest/build-info.json"
  printf '{\n  "rid": "%s",\n  "target": "%s",\n  "library": "%s",\n  "librarySha256": "%s",\n  "ffiVersion": "%s",\n  "commit": "%s",\n  "branch": "%s",\n  "dirty": %s,\n  "rustc": "%s",\n  "builtAt": "%s",\n  "buildHost": "%s",\n  "minimumOs": "%s",\n  "hardwareVideo": "%s"\n}\n' \
    "$rid" "$target" "$lib" "$(rk_sha256 "$dest/$lib")" "$(rk_json "$(rk_ffi_version)")" \
    "$(rk_json "$(rk_git_field commit)")" "$(rk_json "$(rk_git_field branch)")" "$(rk_git_field dirty)" \
    "$(rk_json "$(cd "$ROOT" && rustc --version)")" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$(rk_json "$(uname -srm)")" "$(rk_json "$min_os")" "$(rk_json "$hw")" > "$f.partial"
  mv -f "$f.partial" "$f"
  rk_info "build info: $f"
}

rk_install_lib() {
  local src="$1" dest="$2" lib
  lib="$(basename "$src")"
  mkdir -p "$dest"
  cp -f "$src" "$dest/$lib.partial"
  mv -f "$dest/$lib.partial" "$dest/$lib"
  rk_info "installed: $dest/$lib"
}

rk_package() {
  local rid="$1" lib_path="$2" asset sha sums work lib
  asset="$(rk_asset "$rid")" || rk_die "unknown RID $rid"
  lib="$(basename "$lib_path")"
  [ -f "$ROOT/livekit-ffi/WEBRTC_LICENSE.md" ] || rk_die "livekit-ffi/WEBRTC_LICENSE.md is missing; livekit-ffi/build.rs writes it during the build"
  command -v zip >/dev/null 2>&1 || rk_die "zip is not on PATH (needed for --zip)"
  work="$(mktemp -d)"
  cp "$lib_path" "$work/$lib"
  cp "$ROOT/livekit-ffi/include/livekit_ffi.h" "$work/livekit_ffi.h"
  {
    echo "# livekit"
    echo '```'
    cat "$ROOT/LICENSE"
    echo '```'
    cat "$ROOT/livekit-ffi/WEBRTC_LICENSE.md"
  } > "$work/LICENSE.md"
  rm -f "$DIST/$asset"
  (cd "$work" && zip -X -q "$DIST/$asset" "$lib" livekit_ffi.h LICENSE.md)
  rm -rf "$work"
  sha="$(rk_sha256 "$DIST/$asset")"
  sums="$DIST/SHA256SUMS"
  {
    if [ -f "$sums" ]; then awk -v a="$asset" '$2 != a' "$sums"; fi
    printf '%s  %s\n' "$sha" "$asset"
  } > "$sums.partial"
  mv -f "$sums.partial" "$sums"
  rk_info "zip: $DIST/$asset"
  rk_info "sha256: $sha (recorded in $sums)"
  rk_info "desktop ffi-checksums.sha256 line: $sha  rootapp/$asset"
}

rk_handoff_hint() {
  rk_log "next: install into the desktop repo"
  rk_info "ROOTAPP_FFI_RIDS=\"$*\" ROOTAPP_FFI_LOCAL_DIR=\"$DIST\" bash external/LivekitRtc/download_ffi.sh --source rootapp --force$(for r in "$@"; do printf ' --rid %s' "$r"; done)"
}
