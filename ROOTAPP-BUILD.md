# RootApp livekit-ffi builds

Branch `rootapp/mf-hw-video` is `livekit-ffi/v0.12.76` (2d9f01ab) plus RootApp's hardware codec,
publish-option and build work. The desktop client loads the result through the vendored C# binding in
`external/LivekitRtc`. Every platform builds from the same branch, so SDK-level fixes (simulcast
layers, stereo, encoder fallback, and whatever lands next) reach Windows, macOS and Linux alike.

The fork lives only on Jesse's machines for now: `origin` is upstream `livekit/rust-sdks`, and nothing
is pushed. Everything below works locally. [Release mode](#release-mode-needs-jesses-approval)
describes the GitHub side for when a fork repo is approved.

## Outputs

| RID | Rust target | Library | Script | Build host | Runs on | Hardware video |
|---|---|---|---|---|---|---|
| win-x64 | `x86_64-pc-windows-msvc` | `livekit_ffi.dll` | `build-rootapp-windows.ps1` | Windows x64 | Windows 10+ | MediaFoundation H.264 |
| win-arm64 | `aarch64-pc-windows-msvc` | `livekit_ffi.dll` | `build-rootapp-windows.ps1` | Windows x64 (cross) | Windows 11 ARM64 | MediaFoundation H.264 |
| osx-arm64 | `aarch64-apple-darwin` | `liblivekit_ffi.dylib` | `build-rootapp-macos.sh` | any Mac | macOS 11.0+ | VideoToolbox |
| osx-x64 | `x86_64-apple-darwin` | `liblivekit_ffi.dylib` | `build-rootapp-macos.sh` | any Mac | macOS 10.15+ | VideoToolbox |
| linux-x64 | `x86_64-unknown-linux-gnu` | `liblivekit_ffi.so` | `build-rootapp-linux.sh` | Ubuntu 22.04 x86_64 | glibc of the build host (2.35 max) | NVENC/NVDEC, VAAPI |
| linux-arm64 | `aarch64-unknown-linux-gnu` | `liblivekit_ffi.so` | `build-rootapp-linux.sh` | Ubuntu 22.04 arm64, or x86_64 cross | glibc of the build host (2.35 max) | software (NVENC/NVDEC with `ARM64_NVIDIA=1`) |

All three scripts write the same layout (default `rootapp-dist/` in the repo root, ignored by git):

```
rootapp-dist/
  <rid>/native/<library>          the library the desktop installs
  <rid>/native/build-info.json    provenance, informational
  ffi-<os>-<arch>.zip             only with --zip (-Zip on Windows)
  SHA256SUMS                      "<sha256>  <asset>" for every zip written so far
```

The zips use upstream's release asset names and layout (`.github/workflows/ffi-builds.yml`):
`ffi-windows-x86_64.zip`, `ffi-windows-arm64.zip`, `ffi-macos-x86_64.zip`, `ffi-macos-arm64.zip`,
`ffi-linux-x86_64.zip` and `ffi-linux-arm64.zip`. Each holds three files at its root: the library,
`livekit_ffi.h`, and `LICENSE.md` (this repo's LICENSE followed by the libwebrtc license).

`build-info.json` fields, all strings except `dirty`:

| Field | Example |
|---|---|
| `rid`, `target`, `library` | `osx-arm64`, `aarch64-apple-darwin`, `liblivekit_ffi.dylib` |
| `librarySha256` | sha256 of the installed library |
| `ffiVersion` | `0.12.76` (from `livekit-ffi/Cargo.toml`) |
| `commit`, `branch`, `dirty` | fork commit and branch; `dirty` is true with uncommitted tracked changes |
| `rustc` | `rustc 1.97.1 (...)` |
| `builtAt` | UTC, `2026-10-03T12:00:00Z` |
| `buildHost` | `uname -srm`, or the Windows version |
| `minimumOs` | `macOS 11.0`, `glibc 2.35`, `Windows 10` |
| `hardwareVideo` | `VideoToolbox`, `NVENC/NVDEC, VAAPI`, `MediaFoundation H.264`, `none (software codecs)` |

## Getting the fork onto a Mac or Linux machine

Use a git bundle. Don't copy the Windows working tree: it has CRLF line endings and a Windows `target/`.

```powershell
# Windows, in C:\Users\daerc\Documents\GitHub\livekit-rust-sdks
git bundle create rootapp-fork.bundle ^2d9f01ab rootapp/fx-int
```

```bash
# Mac or Linux
git clone https://github.com/livekit/rust-sdks.git livekit-rust-sdks
cd livekit-rust-sdks
git fetch /path/to/rootapp-fork.bundle 'refs/heads/rootapp/*:refs/heads/rootapp/*'
git checkout rootapp/fx-int
git submodule update --init --recursive
test -x ./build-rootapp-macos.sh && test -f ./build-rootapp-common.sh || echo "WRONG BRANCH: no build-rootapp-macos.sh/build-rootapp-common.sh"
```

The bundle holds only the fork's commits, so it stays small. 2d9f01ab is on upstream `main`. The
submodules (`livekit/protocol`, `libyuv`) come from their public upstreams. Bundle rootapp/fx-int
until the fx-* branches are merged into rootapp/mf-hw-video. That branch has no Mac script, and its
Linux script ignores --check. To pick up later fork commits, create a new bundle with the same
command and fetch it again.

## macOS

Run it on any Mac. Apple silicon builds the x86_64 dylib by cross-compiling, and Intel builds the
arm64 one the same way, just as upstream builds both on `macos-latest`.

What the Mac needs:

- Xcode 16 or newer, or just the command line tools (`xcode-select --install`). These provide Apple
  clang, the macOS SDK, `libclang.dylib` for bindgen, and `git`, `lipo`, `otool`, `nm`, `codesign`,
  `install_name_tool` and `zip`. The script warns when the SDK is older than 15. Upstream links with
  SDK 26.x, and libwebrtc's ScreenCaptureKit/AVFoundation references need a current SDK.
- rustup. If it's missing, the script installs it to `~/.cargo`. The toolchain comes from
  `rust-toolchain.toml` (1.97.1), with the `aarch64-apple-darwin` and `x86_64-apple-darwin` targets.
- protoc 3.15 or newer, from `PROTOC`, from `PATH` (`brew install protobuf`), or downloaded by the
  script (25.2, the version CI uses) to `~/.local/protoc-25.2`.
- No Homebrew, CMake, Ninja, Python or Docker.
- About 20 GB of free disk and network access. The first build of each target downloads crates and
  libwebrtc's prebuilt from the `livekit/rust-sdks` GitHub releases.

```bash
./build-rootapp-macos.sh --check            # prerequisites and plan, builds nothing
./build-rootapp-macos.sh                    # osx-arm64 and osx-x64
./build-rootapp-macos.sh --rid osx-arm64    # one RID (repeat --rid, or comma-separate)
./build-rootapp-macos.sh --zip              # also write ffi-macos-*.zip and SHA256SUMS
./build-rootapp-macos.sh --out /some/dir    # or ROOTAPP_DIST_DIR=/some/dir
```

Per RID, the script:

1. Builds with Apple clang (`xcrun --find clang`, which overrides `CC`/`CXX`):
   `MACOSX_DEPLOYMENT_TARGET=11.0` (arm64) or `10.15` (x86_64), then
   `cargo build --release -p livekit-ffi --target <triple>`. That means default features
   (`rustls-tls-native-roots`) and the workspace release profile (`opt-level="z"`, LTO, one codegen
   unit, `panic=abort`, `strip="symbols"`), the same as upstream's macOS jobs. `.cargo/config.toml`
   adds `-ObjC`. The script ignores `RUSTFLAGS`, because setting it would drop that flag.
2. Sets the install name to `@rpath/liblivekit_ffi.dylib` and signs the dylib ad hoc, since any edit
   voids the linker signature. Upstream leaves rustc's absolute build path as the id. .NET loads the
   dylib by path, so nothing depends on the id.
3. Checks the result and stops if any check fails: `lipo -archs` must be exactly the RID's
   architecture, `otool -l` minos must equal the deployment target, and the id must be `@rpath/...`.
   `otool -L` must list only `/usr/lib` and `/System/Library` entries, which catches Homebrew
   libraries that slip in. The dylib must export `_livekit_ffi_initialize`, contain the desktop's
   probe strings `livekit_ffi_initialize` and `VTDecompressionSession`, and pass
   `codesign --verify --strict`.
4. Installs `rootapp-dist/<rid>/native/liblivekit_ffi.dylib` and `build-info.json`, and the zip with
   `--zip`.

The desktop's Mac publish signs the app bundle, which replaces the ad hoc signature.

## Linux

Build natively on Ubuntu 22.04. Its glibc 2.35 is the desktop app's floor (`GLIBC_FLOOR` in the
desktop's `publish-linux.yml`). Debian 12 and other apt distros also work, with that distro's glibc as
the floor. No Docker.

```bash
./build-rootapp-linux.sh --check                  # prerequisites and plan, builds nothing
./build-rootapp-linux.sh                          # the host's RID
./build-rootapp-linux.sh --zip                    # also write ffi-linux-<arch>.zip and SHA256SUMS
./build-rootapp-linux.sh --rid linux-arm64        # on x86_64: cross build, see below
./build-rootapp-linux.sh --no-apt                 # or SKIP_APT=1: install nothing
```

What the machine needs. The script installs everything except the CUDA header:

- apt packages, installed with sudo unless `--no-apt`: `build-essential pkg-config curl unzip zip
  xz-utils git ca-certificates file binutils lld libssl-dev libglib2.0-dev libx11-dev libxext-dev
  libxfixes-dev libxdamage-dev libxrandr-dev libxcomposite-dev libgl1-mesa-dev libdrm-dev libgbm-dev
  libasound2-dev libpulse-dev`, plus `libva-dev` on x86_64. On a distro without apt, install the
  equivalents yourself and pass `--no-apt`.
- `cuda.h` for NVENC/NVDEC (linux-x64), which the script does not install because it can touch
  driver packages. Choose one:
  - `sudo apt-get install --no-install-recommends nvidia-cuda-dev` (Ubuntu multiverse). If apt
    complains that `libcuda1` is a virtual package, also name the `libnvidia-compute-<N>` that
    matches the installed driver.
  - Install NVIDIA's CUDA toolkit to `/usr/local/cuda`, or set `CUDA_HOME`.

  If the NVIDIA driver came from NVIDIA's `.run` installer, use the toolkit route so apt doesn't
  install a second copy of the driver libraries. Only the header is used at build time.
  `libcuda`, `libnvcuvid` and `libnvidia-encode` are dlopened at runtime, so the result loads on
  machines without NVIDIA.
- clang 21 or newer. libwebrtc.a is built against Chromium's hermetic libc++, and
  `webrtc-sys/build.rs` enforces the minimum. Unless `CC`/`CXX` point at one, the script installs
  LLVM 21.1.8 to `/opt/llvm-21.1.8` with `.github/scripts/install-clang.sh` (sudo). `LIBCLANG_PATH`
  for yuv-sys bindgen comes from the same tree.
- rustup, installed to `~/.cargo` if missing, with the pinned 1.97.1 toolchain and the target.
- protoc 3.15 or newer. Ubuntu's `protobuf-compiler` is 3.12, so the script downloads 25.2 to
  `~/.local` unless `PROTOC` is set.
- About 25 GB of free disk and network access.

Hardware codecs. `webrtc-sys/build.rs` detects them from headers:

| | linux-x64 | linux-arm64 |
|---|---|---|
| NVENC/NVDEC | required (cuda.h). `ALLOW_NO_NVIDIA=1` builds without it | off, like the official build. `ARM64_NVIDIA=1` builds it (needs cuda.h) |
| VAAPI | required (libva-dev). `ALLOW_NO_VAAPI=1` builds without it | not built: webrtc-sys only builds VAAPI for x86_64, as upstream does |
| Jetson MMAPI | n/a | refused, because webrtc-sys would hard-link libnvv4l2/libnvbufsurface/libv4l2. `ALLOW_JETSON=1` makes a Jetson-only build |

A linux-x64 build without both NVIDIA and VAAPI still completes, with a warning. The desktop's
`download_ffi.sh` won't install it, because it requires `NVIDIA H264 Decoder` and `VAAPI H264 Encoder`
in every linux-x64 library. `webrtc-sys/build.rs` doesn't rerun when CUDA or libva appear or
disappear, so the script records the codec configuration in
`target/<triple>/release/.rootapp-hw-config` and runs `cargo clean -p webrtc-sys` when it changes.

### glibc

A native build needs at least the build host's glibc: the result requires the highest `GLIBC_x.y`
symbol version it references. The script prints that version and records it as `minimumOs` in
`build-info.json`. It fails when the version is above `MAX_GLIBC` (default 2.35, the desktop floor).
On Ubuntu 22.04 expect 2.34 or 2.35, because pthread and dl symbols moved into libc in 2.34. On a newer
distro such as Ubuntu 24.04 (2.39), the result typically needs 2.38 and fails the check. Use
`MAX_GLIBC=none` there for a build that only runs on that machine, and never ship it. Upstream's own
`.so` is built in manylinux_2_28 for glibc 2.28. That doesn't matter for Root, whose Native AOT
executable already needs 2.35.

### Checks

The script fails unless the built `.so` passes all of these:

- `readelf -h` reports the right machine (X86-64 or AArch64).
- `livekit_ffi_initialize` is exported.
- `NEEDED` lists only `libc`, `libm`, `libdl`, `libpthread`, `librt`, `libgcc_s` and `ld-linux`. GPU,
  X11 and audio libraries must be dlopened.
- No undefined `cu*`, `cuvid*`, `va*`, `Nv*`, `X*`, `xcb_*`, `drm*` or `gbm_*` symbols. Any of these
  means the lazy-load trampolines in `webrtc-sys/src/lazy_load_deps_for` miss a symbol that a newer
  `cuda.h` or `va.h` uses, and the call would fail at runtime.
- The glibc floor is within `MAX_GLIBC`.
- The desktop's probe strings are present: `livekit_ffi_initialize` and
  `Invalid mix of IDR and non-IDR slices`. With NVIDIA: `NVIDIA H264 Decoder`/`Encoder`,
  `libcuda.so.1`, `libnvcuvid.so.1` and `libnvidia-encode.so.1`. With VAAPI: `VAAPI H264 Encoder`,
  `libva.so.2` and `libva-drm.so.2`.

`LK_DISABLE_NVDEC` (any value) turns NVDEC off at runtime.

### linux-arm64 from an x86_64 host (untested)

A native arm64 host is the tested path: an arm64 VM, a Raspberry Pi 5 or Ampere box with Ubuntu 22.04,
or the CI. To cross-compile on Ubuntu 22.04 x86_64, add arm64 as a foreign architecture. arm64
packages come from `ports.ubuntu.com`, so limit the existing sources to amd64:

```bash
sudo dpkg --add-architecture arm64
sudo sed -i 's/^deb http/deb [arch=amd64] http/' /etc/apt/sources.list
echo 'deb [arch=arm64] http://ports.ubuntu.com/ubuntu-ports jammy main universe
deb [arch=arm64] http://ports.ubuntu.com/ubuntu-ports jammy-updates main universe
deb [arch=arm64] http://ports.ubuntu.com/ubuntu-ports jammy-security main universe' | sudo tee /etc/apt/sources.list.d/arm64-ports.list
sudo apt-get update
sudo apt-get install -y gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross libglib2.0-dev:arm64
./build-rootapp-linux.sh --rid linux-arm64
```

On Pop!_OS or 24.04-style deb822 sources, add `Architectures: amd64` to the existing `.sources` files
instead of running the `sed`. The script links with `aarch64-linux-gnu-gcc` plus `-fuse-ld=lld` from
`.cargo/config.toml`, and compiles with clang 21 for `aarch64-unknown-linux-gnu`. It reads the arm64
`.pc` files from `/usr/lib/aarch64-linux-gnu/pkgconfig` (override with `CROSS_PKGCONFIG_DIR`) with
`PKG_CONFIG_ALLOW_CROSS=1`. The glibc floor is the host's `libc6-dev-arm64-cross` version, which
matches the distro's glibc. `g++-aarch64-linux-gnu` is needed even though clang compiles: the `cxx`
crate's runtime and `link-cplusplus` build against the arm64 libstdc++ headers and link `-lstdc++`.

## Windows

Requirements: Visual Studio or Build Tools with the x64 MSVC toolset and a Windows 10/11 SDK, rustup
(the script installs the pinned toolchain and target), and a tools folder. The default tools folder is
`%USERPROFILE%\dev-tools`, holding `protoc\bin\protoc.exe` and an LLVM tree under `llvm\`
(libclang.dll, clang.exe, clang-cl.exe, lld-link.exe, llvm-lib.exe, `lib\clang\*`). `-ToolsDir`,
`-LlvmDir` and `-Protoc` point elsewhere. CI passes `-LlvmDir "C:\Program Files\LLVM"` and the
setup-protoc binary. `webrtc-sys` downloads the target's libwebrtc prebuilt on the first build.

```powershell
.\build-rootapp-windows.ps1                                   # win-x64 and win-arm64
.\build-rootapp-windows.ps1 -Targets x86_64-pc-windows-msvc
.\build-rootapp-windows.ps1 -Targets aarch64-pc-windows-msvc -Arm64CrtLibDir "$env:USERPROFILE\dev-tools\xwin\crt\lib\aarch64"
.\build-rootapp-windows.ps1 -Targets aarch64-pc-windows-msvc -CompileOnly
.\build-rootapp-windows.ps1 -Zip                              # also write ffi-windows-*.zip and SHA256SUMS
```

Each built DLL lands in `rootapp-dist\<rid>\native\` with a `build-info.json`.

Both targets link the CRT statically (`+crt-static` in `.cargo/config.toml`) and delay-load
`mfplat.dll` (`/DELAYLOAD:mfplat.dll` plus `delayimp.lib`, set in `livekit-ffi/build.rs`). The DLL
therefore loads on Windows N/KN without the Media Feature Pack and falls back to software codecs
there.

### libyuv SIMD on win-x64

libyuv only compiles its x86 SSSE3/AVX2 row functions for GCC-style compilers, so a
`cl.exe` build of `yuv-sys` runs every FFI colour conversion (BGRA to I420 for each
captured camera and screen frame) through the scalar C rows. For the x64 target the
script sets `LK_YUV_CC` to the LLVM `clang-cl.exe`, and `yuv-sys/build.rs` compiles
libyuv with it. Only yuv-sys changes compiler: `CC_x86_64_pc_windows_msvc` would also
move `ring` and `soxr-sys`. Without `LK_YUV_CC` (upstream, or a build outside the
script) yuv-sys uses `cl.exe` as before. libyuv picks AVX2/SSSE3 at runtime and falls
back to C on CPUs without them; the output is bit-identical to the C rows. Check a build
with:

```powershell
dumpbin /LINKERMEMBER:1 target\x86_64-pc-windows-msvc\release\build\yuv-sys-*\out\yuv.lib | Select-String rs_ARGBToYRow_AVX2
```

### win-arm64 on an x64 machine

If an MSVC toolset has `bin\Hostx64\arm64\cl.exe`, cargo/cc-rs use it directly. Otherwise the script
cross-compiles with LLVM and sets these variables for that target only:

| Variable | Value |
|---|---|
| `CC_aarch64_pc_windows_msvc`, `CXX_aarch64_pc_windows_msvc` | `clang-cl.exe` |
| `AR_aarch64_pc_windows_msvc` | `llvm-lib.exe` |
| `CFLAGS_/CXXFLAGS_aarch64_pc_windows_msvc` | `--target=aarch64-pc-windows-msvc -DLIBYUV_DISABLE_NEON -DLIBYUV_DISABLE_SVE -DLIBYUV_DISABLE_SME` |
| `INCLUDE` | MSVC `include` + Windows SDK `ucrt;um;shared;winrt;cppwinrt` (headers are arch-neutral) |
| `CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_LINKER` | `lld-link.exe` |
| `CARGO_ENCODED_RUSTFLAGS` | `+crt-static` and `/LIBPATH:` for the ARM64 CRT and SDK `ucrt\arm64`, `um\arm64` |
| `PATH` | LLVM `bin` first, because `ring` switches to `clang` on Windows ARM64 |

cc-rs can't find an x64-hosted ARM64 `cl.exe`, so it doesn't inject a VS environment for clang-cl.
That's why the script sets `INCLUDE` explicitly. It restores the variables after the arm64 build, so a
later x64 build in the same run is unaffected. clang-cl defines `__aarch64__`, which turns on libyuv's
NEON/SVE/SME row functions, but yuv-sys never compiles their assembly sources for Windows. The
`LIBYUV_DISABLE_*` defines match the official MSVC build, which leaves them off.

The link still needs the **ARM64 MSVC runtime libraries** (`libcmt.lib`, `libvcruntime.lib`,
`libcpmt.lib`, `oldnames.lib`, `delayimp.lib`). They ship with the VS component *MSVC ARM64/ARM64EC
build tools* (`Microsoft.VisualStudio.Component.VC.Tools.ARM64`). A VS install without that component
has only the ASan libraries in `VC\Tools\MSVC\<ver>\lib\arm64`. If the libraries live elsewhere, pass
`-Arm64CrtLibDir <dir>` or set `LK_ARM64_CRT_LIB_DIR`. One option is an `xwin splat` output: xwin
0.10.0 with `--accept-license --arch aarch64 splat --disable-symlinks`, using `crt\lib\aarch64`.
Without the libraries the script stops before cargo with that message.

The script compiles against the MSVC headers that belong to those libraries. A toolset's `lib\arm64`
pairs with that toolset's `include`. An xwin splat's `crt\lib\aarch64` pairs with its `crt\include`,
plus its `sdk\include` and `sdk\lib` when present. For any other folder the script warns and falls
back to the newest installed toolset's headers. Those must be the same MSVC version as the libraries,
or the link fails on `__std_*` / vcruntime symbols. `-CompileOnly` still builds
`target\aarch64-pc-windows-msvc\release\livekit_ffi.lib` (a staticlib, no link step), which proves
that every C/C++/Rust unit compiles for ARM64.

CI gets the CRT from the runner image: GitHub's Windows images include the VS ARM64 build tools, so
the script takes the plain MSVC path. If an image ever drops them, the workflow's "ARM64 MSVC runtime
libraries" step downloads xwin 0.10.0 (sha256-pinned) and splats the CRT. It then exports
`LK_ARM64_CRT_LIB_DIR`, which switches the script to the clang-cl path above.

Check an arm64 DLL with `dumpbin` (under `VC\Tools\MSVC\<ver>\bin\Hostx64\x64`):

```powershell
dumpbin /headers livekit_ffi.dll | Select-String machine   # AA64 machine (ARM64)
dumpbin /imports livekit_ffi.dll                           # "delay load imports" lists mfplat.dll
```

### Runtime switches (Windows MediaFoundation)

The engine reads these through `GetEnvironmentVariableA`, so values the host sets after the DLL loads
still apply. Flags accept `1/true/yes/on`. `LK_MF_ALLOW_AMD` also accepts `0/false/no/off`.

| Variable | Effect |
|---|---|
| `LK_DISABLE_MF_ENCODE=1` | The MF encoder factory reports no H.264 encoder, so encoding uses OpenH264. |
| `LK_DISABLE_MF_DECODE=1` | The MF decoder factory reports no H.264 decoder, so decoding uses FFmpeg. |
| `LK_MF_ENCODER_ADAPTER=nvidia\|amd\|intel` | Try that vendor's encoder MFT first. |
| `LK_MF_ALLOW_AMD=0` | Skip AMD encoder MFTs (browser-parity workaround for AMD CBP black remote video). Allowed by default. |

The library's other support switches are `LK_FFI_LOG_LEVEL` (see Logging) and
`LK_FFI_WORKER_THREADS` (see Tokio runtime size). The D3D11 sharing A/B switches
(`LK_MF_D3D11_SHARING`, `LK_MF_D3D11_DRIVER_THREADING`) were removed in `rootapp/prod-cleanup`.

Fault injection, for testing recovery only. A release library does not read these: they are
compiled in only with webrtc-sys's `mf-fault-injection` feature (off by default; the release
workflow and the build scripts never turn it on). webrtc-sys's own tests turn it on through a
dev-dependency on itself. For a DLL that reads them, build with
`cargo build --release -p livekit-ffi --target x86_64-pc-windows-msvc --features webrtc-sys/mf-fault-injection`
(the variables are read once per process, except `LK_MF_FAULT_HOST_DEVICE_REMOVED`, which is read on
every host acquire):

| Variable | Effect |
|---|---|
| `LK_MF_FAULT_INIT=1` | MF H.264 `InitEncode` fails, which must fall back to software. |
| `LK_MF_FAULT_AFTER_FRAMES=N` | The encoder simulates device removal after N frames. |
| `LK_MF_MAX_SESSIONS=N` | Refuse to open more than N concurrent MF encoder sessions. A session counts until its transform's delayed final release; an encoder at the limit waits for closing sessions before it is refused. |
| `LK_MF_FAULT_RUNTIME_RC=1` | Runtime VBV/max bitrate updates fail, as if the VBV were fixed at configuration (what NVIDIA does silently). |
| `LK_MF_FAULT_STRICT_RC=1` | Enforce strict rate-control ordering, like drivers that reject max < mean or an undersized VBV. |
| `LK_MF_FAULT_INIT_BPS=N` | Every MF encoder sizes its rate control for N bps at InitEncode, whatever its start bitrate. `30000` reproduces a simulcast top layer started at its placeholder minimum; `300000` a low start that the starvation re-init has to fix. |
| `LK_MF_FAULT_DECODE_AFTER_FRAMES=N` | Decoder fault after N frames. |
| `LK_MF_FAULT_DECODE_MODE=unlisted\|renegotiate` | Decoder fault kind. The default is `DXGI_ERROR_DEVICE_REMOVED`, `unlisted` is `E_FAIL`, and `renegotiate` is `MF_E_TRANSFORM_STREAM_CHANGE`. |
| `LK_MF_FAULT_HOST_DEVICE_REMOVED=1` | `livekit_ffi_d3d11_acquire_shared_device` reports the shared device as removed, so the host falls back to a device of its own. |

## Handing a build to the desktop repo

Local mode works the same on every OS. In a RootApp.Client.Desktop checkout on the machine that will
run the app:

```bash
ROOTAPP_FFI_RIDS="osx-arm64" \
ROOTAPP_FFI_LOCAL_DIR=/path/to/livekit-rust-sdks/rootapp-dist \
  bash external/LivekitRtc/download_ffi.sh --source rootapp --force --rid osx-arm64
```

Each script prints this command for the RIDs it built. `download_ffi.sh` copies
`<dir>/<rid>/native/<lib>` into `external/LivekitRtc/runtimes/<rid>/native/`, verifies the
architecture and probe strings, and records `"source": "rootapp", "origin": "local-build",
"verified": false` in `ffi-manifest.json`. Nothing pins a local build's sha256. `ROOTAPP_FFI_RIDS`
selects which RIDs come from the fork; set it until the desktop's `ffi-sources.conf` lists the RID.
`--source rootapp` makes a missing fork build an error instead of a silent fallback to the official
library.

If `<dir>` holds `ffi-<os>-<arch>.zip` and no `<rid>/native/<lib>`, the script takes the zip
(`origin: local-zip`) instead. That requires a matching `rootapp/<asset>` line in
`external/LivekitRtc/ffi-checksums.sha256`. The build scripts print that line, and `SHA256SUMS` holds
the hashes.

A `rootapp-dist` can move between machines (USB, scp). Its layout is the same everywhere.

## Release mode (needs Jesse's approval)

Nothing here has been done. Creating the repo, pushing and tagging are Jesse's calls.

1. Create a **private** repository for the fork, for example `<org>/livekit-rust-sdks`. A GitHub fork
   of a public repo can't be private, so make a new repo and push to it instead of using the Fork
   button.
2. In the new repo's Actions settings, keep only `RootApp FFI` (`.github/workflows/rootapp-ffi.yml`)
   enabled and disable the upstream workflows. Those run on pushes and pull requests to `main`, and
   `ffi-builds.yml` manages `livekit-ffi/*` releases. In a private repo, macOS minutes bill at 10x.
3. Push the branch: `git remote add rootapp <url>` then `git push rootapp rootapp/mf-hw-video`.
4. Build a release in one of two ways:
   - Push a tag: `git tag rootapp-ffi/v0.12.76-hw.1 && git push rootapp rootapp-ffi/v0.12.76-hw.1`.
     That builds all six RIDs and publishes the release.
   - Run **Actions → RootApp FFI → Run workflow**, with an optional `tag`, `rids` and `publish`. Without
     a tag it only uploads workflow artifacts, and without `publish` the release stays a draft.
   Tags look like `rootapp-ffi/v<ffi version>-hw.<n>`. A run with a tag must build all six RIDs, since
   `SHA256SUMS` only lists the zips that run built. It also refuses a tag that already points to another
   commit, or a draft release that targets one, because the release would then hold binaries that don't
   match its tag.
5. The release gets the six zips, `SHA256SUMS` and `build-info-<rid>.json`. The release job checks
   that each zip has the library, `livekit_ffi.h` and `LICENSE.md` at its root, and that `file` reports
   the asset's architecture. The job summary lists the lines for the desktop's checksum file.
6. In RootApp.Client.Desktop:
   - Add the summary's `<sha256>  rootapp/<asset>` lines to `external/LivekitRtc/ffi-checksums.sha256`.
   - In `ffi-sources.conf`, set `ROOTAPP_FFI_TAG`, `ROOTAPP_FFI_REPO=<org>/<repo>`, and
     `ROOTAPP_FFI_RIDS` to the six RIDs.
   - Give every workflow that runs `download_ffi.sh` a `GH_TOKEN` that can read the private repo.
     The Linux container also needs `gh`.
   - Optionally set `ROOTAPP_FFI_SOURCE: rootapp` so a missing fork asset fails the build.

The workflow needs no secrets beyond `GITHUB_TOKEN`. It only creates releases from a job with
`contents: write`, and it does nothing in a repository owned by `livekit`. Its jobs:

| Job | Runner | How |
|---|---|---|
| win-x64, win-arm64 | `windows-latest` | `build-rootapp-windows.ps1 -Targets <triple> -LlvmDir "C:\Program Files\LLVM" -Protoc <setup-protoc> -Zip` |
| osx-arm64, osx-x64 | `macos-latest` | `build-rootapp-macos.sh --rid <rid> --zip` |
| linux-x64 | `ubuntu-24.04` + container `ubuntu:22.04` | installs `nvidia-cuda-dev` (picking a `libnvidia-compute` provider), then `build-rootapp-linux.sh --rid linux-x64 --zip` |
| linux-arm64 | `ubuntu-24.04-arm` + container `ubuntu:22.04` | `build-rootapp-linux.sh --rid linux-arm64 --zip` |

The Ubuntu 22.04 container pins the glibc floor at 2.35, the desktop's floor, whatever the runner
image is.

## Cross-platform audit

### Fork changes up to e2a6b46b

The audit covered `2d9f01ab..e2a6b46b` on 2026-10-03 and found nothing that breaks a macOS or Linux build:

- The MediaFoundation sources (`webrtc-sys/src/mf/*`), their system libraries (`mfplat`, `mfuuid`, `mf`,
  `dxguid`) and the MSVC flags (`/DUSE_MF_VIDEO_CODEC=1`) are all in the `"windows"` arm of
  `webrtc-sys/build.rs`.
- `/DELAYLOAD:mfplat.dll` in `livekit-ffi/build.rs` is gated on `target_os = "windows"` and
  `target_env = "msvc"`.
- The shared C++ only touches MF behind `#if defined(USE_MF_VIDEO_CODEC)`: `video_encoder_factory.cpp`,
  `video_decoder_factory.cpp`, `rtp_sender.cpp` and `include/livekit/*.h`. The rest is portable
  (`std::map`, `std::mutex`, `webrtc::TaskQueueBase`).
- The Rust changes (`livekit/src/room/options.rs`, `rtc_engine/*`, `livekit-ffi/src/conversion/room.rs`,
  proto fields 100/101) are platform-independent.
- No `Cargo.toml` changes, so no new crates on any target.

Some shared changes affect behavior on macOS and Linux too, which is worth knowing when testing there:

- H.264 hardware encoder layers (VideoToolbox, NVENC, VAAPI) now get a software fallback encoder from
  `SoftwareFallbackFactory`. That's OpenH264 when the libwebrtc build has it (`WEBRTC_USE_H264`), and
  otherwise nothing, the same as before.
- Hardware H.264 decoders (VideoToolbox, NVDEC) are wrapped with the FFmpeg H.264 decoder as a fallback
  when the internal decoder passes its startup probe. The macOS prebuilt's FFmpeg is expected to lack
  H.264, which would make this a no-op on macOS.
- A sender's encoder selector now overrides the format's backend tag per encoder task queue.
- `simulcast_layers` (100) and `force_stereo` (101) are honored on every platform.

### Checklist for later fork changes

This applies to `rootapp/fx-encoder`, `rootapp/fx-session` and later branches. Before merging into
`rootapp/mf-hw-video`, check:

1. **Sources per OS.** New C/C++ files go into the matching `match target_os` arm of
   `webrtc-sys/build.rs`, never the shared `builder.files(...)` list, unless they compile for every
   target (Windows, macOS, Linux, iOS, Android). Windows-only code (MF, D3D11/DXGI, COM/WRL, a shared D3D device) stays
   in the `"windows"` arm and `src/mf/`.
2. **Guarded includes.** Shared files (`video_encoder_factory.cpp`, `video_decoder_factory.cpp`,
   `rtp_sender.cpp`, `peer_connection_factory.cpp`, `include/livekit/*.h`) include platform headers
   only under `#if defined(USE_MF_VIDEO_CODEC)` / `_WIN32` / `__APPLE__` / `USE_NVIDIA_VIDEO_CODEC` /
   `USE_VAAPI_VIDEO_CODEC`. Find unguarded hits with:
   `git diff rootapp/mf-hw-video... -- webrtc-sys/src webrtc-sys/include | grep -nE 'windows\.h|wrl|ComPtr|mfapi|mftransform|d3d11|dxgi|GetEnvironmentVariable|__declspec|#pragma comment'`.
   Every hit must sit in `src/mf/` or inside a guard.
3. **Flags and link args.** MSVC syntax (`/D...`, `/std:c++20`, `/EHsc`, `/wd...`, `/DELAYLOAD`,
   `delayimp`) belongs only to the Windows arm. Linker args additionally need the
   `CARGO_CFG_TARGET_ENV == "msvc"` check. `cargo:rustc-link-lib` for Windows system libraries belongs
   only to the Windows arm.
4. **Linux links nothing new.** On Linux, GPU, X11 and audio libraries must go through
   `add_lazy_load_so` (dlopen trampolines), not `rustc-link-lib`. The Linux script's NEEDED allowlist
   fails the build otherwise. New CUDA/VA symbols must exist in the trampoline tables
   (`webrtc-sys/src/lazy_load_deps_for/*`); the undefined-symbol check catches gaps.
5. **Crates per target.** `windows`, `windows-sys`, `winapi`, `objc2` and `core-foundation` go only
   under `[target.'cfg(...)'.dependencies]`, and code using them sits behind `#[cfg(target_os = ...)]`.
6. **Environment reads.** `GetEnvironmentVariableA` is fine inside `src/mf/`. Shared code uses
   `std::getenv`.
7. **yuv-sys and libyuv builds** (for example the libyuv clang build on `rootapp/fx-encoder`). Keep the
   non-MSVC aarch64 path that macOS arm64 and Linux arm64 use: `yuv_neon64` plus SVE/SME detection. Scope
   any `LIBYUV_DISABLE_*` define or clang-cl flag to Windows (`CARGO_CFG_TARGET_ENV == "msvc"`).
8. **Shared encoder logic** (start-bitrate caps, rate-control, unpublish and receive-path changes) also
   runs with VideoToolbox, NVENC, VAAPI and OpenH264. Don't assume MF semantics such as async MFT
   output, VBV or D3D textures outside `src/mf/`.
9. **Protocol.** New FFI proto fields are numbered 100 or higher. Regenerate the desktop's
   `Proto/*.g.cs` from this branch.
10. **Build config.** Keep the per-target rustflags in `.cargo/config.toml`: `-ObjC` on macOS and
    `-fuse-ld=lld` on aarch64 Linux. Never export `RUSTFLAGS` around a build; the scripts unset it.
11. **Scripts.** `*.sh` stay LF (`.gitattributes`) and bash 3.2 compatible, because macOS runs them
    with `/bin/bash`. Don't use `mapfile`, associative arrays, `${var,,}`, or empty `"${arr[@]}"`
    under `set -u`.
12. **Verification.** Windows can't compile webrtc-sys for macOS or Linux, because its build script
    needs the target's toolchain and sysroot. Merges that touch `webrtc-sys/`, `yuv-sys/`,
    `livekit-ffi/build.rs` or `Cargo.toml` need a real build: run `build-rootapp-macos.sh` /
    `build-rootapp-linux.sh` on the Mac and Linux machines, or the RootApp FFI workflow once the fork
    is on GitHub. Rust-only changes under `livekit/` and `livekit-ffi/src/` carry little risk, but
    still need one platform build before release.

## Unpublish and the publisher SDP

`unpublish_track` keeps upstream's `remove_track` (the m-section goes inactive and stays in
the SDP, as in livekit-client), calls `RtpSender::release_video_encoder` for video (libwebrtc
recreates the sender's `VideoSendStream` without a source, which destroys the encoder instances
at once), and marks the transceiver's mid as released on the publisher transport.

JSEP keeps a sender's msid and SSRCs in every later m-section once it has sent, so the
transceiver's media channel keeps the send stream: before this, every unpublished video track
kept a `VideoEncoderQueue` thread and every unpublished audio track an `AudioEncoderQueue` thread
until the PeerConnection closed (six camera + screen + screen-audio cycles: 18 dead queue
threads). Every later publisher offer now leaves the stream lines (`a=msid`, `a=ssrc`,
`a=ssrc-group`, `a=rid`, `a=simulcast`) out of released m-sections, so `SetLocalDescription`
removes the send stream through libwebrtc's own `BaseChannel::UpdateLocalStreams_w`, and later
offers keep it out. libwebrtc accepts this munging by default; the server sees an inactive
m-section without an msid. In the harness the six cycles left no encoder queue thread behind, and
the subscriber saw every publish and unpublish.

Removing the stream behind libwebrtc's back (`RemoveSendStream` on the media channel) does not
work: the sender's parameters then come back without encodings, the next offer drops
`a=simulcast`, a parsed offer drops the rids, and `SetLocalDescription` fails trying to remove the
stream a second time.

The m-sections themselves still accumulate (three per camera + screen + screen-audio cycle), as
they do in livekit-client; only stopping the transceiver would let libwebrtc recycle them (see
below).

libwebrtc's worker thread, which also delivers incoming audio, waits for the encoder's `Release()`
when the send stream goes away. The MF encoder therefore hands the MFT shutdown and its device references to
one process-wide release thread (`webrtc-sys/src/mf/mf_deferred_release.h`) and returns at once;
a teardown on the worker cost 5-30 ms of concealed incoming audio per camera stop (0.1-0.5 s
without the shared D3D11 device). Opening an encoder session waits for pending teardowns first.

The teardown ends streaming, shuts the asynchronous MFT down through `IMFShutdown` and
`IMFActivate::ShutdownObject`, and releases the last transform references 1.5 s later on a
`DelayedReleaseQueue`, so work items the MFT queued before shutdown have run. Until that final
release:

- The session still counts as open (`webrtc-sys/src/mf/mf_encoder_sessions.h`), since the driver
  may hold the NVENC session until then; the encoder logs `MF encoder sessions open: N` (and
  `(M closing)`) on every open and final release, and `LK_MF_MAX_SESSIONS` (fault injection,
  `mf-fault-injection` builds only) counts closing sessions too.
- When no hardware MFT accepts a new session while sessions are closing, `InitEncode` (or the
  bitrate re-init) still succeeds, `Encode()` drops frames until those sessions' final release
  (at most 2.5 s) and then tries once more before falling back to software. Nothing blocks: the
  encoder's task queue keeps running, so WebRTC's worker thread, which waits for that queue when it
  destroys the stream (unpublish), is not held up.
- The delayed release also holds the encoder's reference to the adapter's shared D3D11 device, so
  a codec opened within the window reuses it instead of creating a second device (35 NVIDIA
  driver threads) while the old one is still alive.

The MF decoder's `Release()` hands its MFT, staging texture and device reference to the same
thread. When a subscribed track goes away the worker waits for it too, and in the same process the
two teardowns used to run at once on the shared device. Because the queue is shared, opening an
encoder session also waits for a pending decoder teardown.

Stopping the transceiver instead is not compatible with LiveKit server 1.13.7:

- libwebrtc recycles a rejected m-section for the next transceiver under a new mid, but the
  server only treats m-sections after the last answered mid as new. A recycled slot that is not
  the last video section gets no simulcast rid mapping (`adding up track failed: duplicate
  layer` for rids h and f). Re-checked on 2026-10-04 against the local 1.13.7 server: a camera
  republished into its recycled m-section reached the subscriber at 320x180 only.
- A rejected m-section keeps its old ICE credentials, so the next ICE-restart offer has
  conflicting `ice-ufrag` values; the server answers with `LEAVE STATE_MISMATCH` and the resume
  escalates to a full reconnect.
## MediaFoundation encoder rate control

NVIDIA's H.264 MFT reads `CODECAPI_AVEncCommonBufferSize` (the VBV) once, when the media
types are set, and returns S_OK for later updates without applying them. Mean bitrate,
max bitrate and `MF_MT_AVG_BITRATE` do follow at runtime. Its frames stay near one VBV in
size, so an encoder configured at a low bitrate cannot reach a higher target. A simulcast
top layer that the first allocation can't fit is initialised at its 30 kbps minimum
(SimulcastEncoderAdapter), at startup and on every dynacast resume, and used to stay at
QP 41-50.

- The VBV is 100 ms of the target, at least three frames at the codec's max frame rate
  (600 ms at 5 fps) and at most 1 s. A 500 ms VBV at the target left 2K simulcast shares
  0.5-1 s behind in the sender's pacer.
- An encoder configured below 150 kbps is rebuilt at its target as soon as the target is
  at least twice that (before its first frame when the layer was paused).
- Otherwise it is rebuilt when the target has stayed at least twice the configured
  bitrate for a second, its frames over the last second averaged at least half the VBV,
  and the last configuration is at least 5 s old. The rebuild releases the old MFT first
  and costs one keyframe. There is no downward rebuild.

The policy is `webrtc-sys/src/mf/mf_reinit_policy.h`; the encoder logs
`MF H264 encoder VBV is sized for ... re-initializing` when it fires.

## Waiting on the shared D3D11 device

Every MF encoder and decoder on an adapter shares one multithread-protected D3D11 device. A
blocking `Map` holds that device's lock until the GPU is done, which stalls every other stream's
D3D calls, so neither side waits inside `Map`:

- The decoder's readback signals a `D3D11GpuFence` (`webrtc-sys/src/mf/mf_gpu_fence.h`) after its
  copy and waits on the fence event, then maps with `D3D11_MAP_FLAG_DO_NOT_WAIT`.
- The encoder uploads through a ring of staging textures. Each slot records the fence value
  signalled after its copy and is mapped with `DO_NOT_WAIT` only once that value has completed.
  When the oldest slot is still busy, the ring grows by one slot, up to 5, and logs
  `MF encoder upload ring grown to N`. At 5 the encoder waits on the fence.
- Without `ID3D11Fence` (before WDDM 2.0) both poll with `DO_NOT_WAIT`. After 200 ms both fall
  back to a blocking `Map`.

## Sharing the D3D11 device with the host (Windows only)

On Windows, `livekit_ffi.dll` exports two C functions so the host can work on the codecs' shared
device instead of creating its own. They are not part of the protobuf API, are not built for any
other target, and are missing from `livekit-ffi/include/livekit_ffi.h` (which is stale anyway):

```c
HRESULT livekit_ffi_d3d11_acquire_shared_device(const LUID* adapter_luid, ID3D11Device** out_device);
HRESULT livekit_ffi_d3d11_release_shared_device(ID3D11Device* device);
```

The contract:

- `adapter_luid` names the DXGI adapter. Null means the default adapter (DXGI adapter 0, the one
  `D3D11CreateDevice(nullptr, ...)` picks).
- Acquire returns the adapter's shared device, the one MF encoders and decoders use. If no codec
  holds one yet, it creates it: a hardware device at feature level 11.1 down to 10.0, created with
  `VIDEO_SUPPORT | BGRA_SUPPORT | PREVENT_INTERNAL_THREADING_OPTIMIZATIONS` (see below) and
  multithread protected, plus its DXGI device manager.
- On success `*out_device` carries one COM reference, which the caller releases with `Release()`
  like any COM out-parameter. Besides that, the FFI keeps a host reference to the device until the
  matching release. While that reference exists, the device stays the adapter's shared device, so
  codecs opened later reuse it and it outlives the last encoder.
- Call release once per successful acquire, from any thread, before or after releasing your own
  COM reference: the pointer is only compared, never used. Release fails with `E_INVALIDARG` when
  no host reference to that device is left. When the host held the last reference, the device and
  its driver threads are torn down on the MF deferred release thread, so release returns at once.
- Acquire never hands out a removed device. It returns the removal reason (`DXGI_ERROR_DEVICE_REMOVED`
  and so on) with `*out_device` null; the next acquire, like the next codec, gets a new device.
- Other failures: `E_POINTER` (null `out_device`), `MF_E_PLATFORM_NOT_INITIALIZED` (no `mfplat.dll`
  on Windows N/KN, or `MFStartup` failed), `DXGI_ERROR_NOT_FOUND` (no adapter with that LUID), or
  the `D3D11CreateDevice` error. The host then creates its own device, as it would against the
  official FFI, which has no such export.
- The immediate context is shared with the encoders' uploads and the decoders' readbacks. The host
  must not block in `Map` (signal a fence or poll with `DO_NOT_WAIT`, as above), must hold
  `ID3D11Multithread::Enter`/`Leave` around call sequences that bind pipeline state, and must not
  turn multithread protection off.
- Driver threads, measured on an RTX 5090 and the Ryzen's Radeon iGPU (drivers of 2026-10): a
  default NVIDIA D3D11 device runs 37-38 driver threads, and the first shader created on it starts
  32 more, a compiler pool with one thread per logical CPU, which lives until the device is
  released. On the shared device that pool would outlive the share that created it for as long as
  any codec, camera or remote video, keeps the device. `PREVENT_INTERNAL_THREADING_OPTIMIZATIONS`
  brings an NVIDIA device to 35 threads with no pool (the scaler's shader compiles in 0.5 ms on the
  calling thread), and an AMD one from 33 to 30 (AMD starts no pool). In a harness call with a
  2K screen share, a 3-layer HW camera and an MF-decoded remote camera, the flag left encode and
  decode times unchanged (screen 3.3-3.5 ms/frame, camera 0.8-1.2, decode 0.11 vs 0.15) and cut
  the capture's arrival-to-delivered latency from 6.8 ms to 1.9 ms at the median.
- Test: `cargo test -p webrtc-sys mf_device` covers the contract on a real adapter and skips on a
  machine without a hardware D3D11 video device. `LK_MF_FAULT_HOST_DEVICE_REMOVED=1` makes acquire
  report the device as removed (fault injection: webrtc-sys's tests build with it, a release
  library never reads the variable).

Until `rootapp/prod-cleanup`, `LK_MF_D3D11_SHARING=off|user` (a device per codec, or one per adapter
for the encoders and the host and another for the decoders) and `LK_MF_D3D11_DRIVER_THREADING=on`
(the driver's threading optimizations back on) kept the behaviour from before the shared device as
an A/B. Both are gone: one device per adapter, always created with
`PREVENT_INTERNAL_THREADING_OPTIMIZATIONS`.

The desktop's Windows screen capture (`WgcCaptureDevice.cs` and `FfiSharedDevice.cs` in
`RootApp.Client.Avalonia.Desktop.Windows/Helpers`) resolves both exports with
`NativeLibrary.TryGetExport` from the already loaded `livekit_ffi.dll`, so it works under Native
AOT. It uses the shared device only when its GPU scaler can run there (feature level 11.0, BGRA,
multithread protection, `ID3D11Device5` fences). Otherwise, with an older DLL, or after any
failure, it creates its own device as before.

## Start bitrate

The publisher transport's first video track sets its start bitrate through
`PeerConnection::SetBitrate`: 0.9 × the sum of its encodings' max bitrates, capped at 1 Mbps
for cameras and 3 Mbps for screen shares. livekit-client leaves screen shares uncapped,
but Chromium writes its value only on the first H.264 payload type (42001f) while the SFU
answers with 42e01f, so the browser really starts at libwebrtc's default. 3 Mbps turns on
a 2K share's top layer (low layer 1.2 Mbps) at the first allocation without starting far
above typical uplinks.

It is applied once per PeerConnection and never as `x-google-start-bitrate`. Every video
send channel hands its codec's start bitrate to the whole call
(`RtpBitrateConfigurator::UpdateWithSdpParameters`), a channel without one clears the
remembered value, and the next channel that applies one resets the established estimate to
it. With the start munged per track, every camera publish or new screen share dropped a
28 Mbps estimate to the start value (`BWE Setting start bitrate to: ...` in the libwebrtc
log) and the new share started soft for 20-25 s.

Every video publish and unpublish also sets the estimate's max through `SetBitrate`: twice the
sum of the published video tracks' encoding max bitrates, at least 5 Mbps. Without a max,
libwebrtc caps every bandwidth probe at 5 Mbps (`kDefaultMaxProbingBitrate`). With simulcast
encodings lowest-first and periodic ALR probing (see the padding notes in the commit history), the
sender no longer pads up to its top layer, so on static content the estimate sat at 5-6 Mbps and a
15 Mbps top layer needed ~14 s of AIMD increase once the content moved. With the max, ALR probes
reach twice the estimate (libwebrtc limits them to twice the allocated bitrate), and in the
harness the top layer reached 14.3 Mbps within a second of a static share turning to motion.

## Tokio runtime size

The FFI's main tokio runtime runs `min(available_parallelism, 8)` workers instead of tokio's one per
logical CPU (32 on a 32-thread machine). `LK_FFI_WORKER_THREADS=N` overrides that on every platform:
it is read once, when the first FFI request creates the server; N is clamped to
`available_parallelism`, and 0 or a value that doesn't parse keeps the default. `TOKIO_WORKER_THREADS`
no longer affects this runtime, because its size is set explicitly; use `LK_FFI_WORKER_THREADS`.
Workers keep the name `tokio-rt-worker`. The blocking pool keeps tokio's on-demand default (DNS,
`tokio::fs`, audio filter `on_load`; idle threads exit after 10 s), and the audio capture runtime stays
at one `livekit-audio` worker. On Windows `available_parallelism` ignores the process affinity mask.

Many FFI paths make synchronous libwebrtc proxy calls on a worker (data channel `Send`, `add_transceiver`,
`remove_track`, `SetParameters`, `GetStats`, `PeerConnection::Close`), so the worker waits on the
signaling or network thread while using no CPU. With one worker a reliable-data flood (about 2.8k
messages/s) backs up for seconds; two workers keep up. Measure with the flood before lowering the
default.

Why 8 and not 4: in a two-room harness (mic, camera, screen and screen audio published on room A and
all subscribed by room B, a third room C sending a tone and data to B) both counts match 32 workers on
an idle machine, including signal, full and resume-failed reconnects. Under CPU contention (harness and
a 16-thread spin burner pinned to the same 16 logical CPUs, both below normal priority) the
resume-failed reconnect, where all four tracks are republished, did not finish within 20 s in 2 of 9
runs with 4 workers and took 5.6 and 7.6 s in 2 more, while room B's `GetStats` stalled for up to 10 s.
With 32 workers 1 of 9 runs took 11.5 s, with 8 workers 1 of 7 took 10.9 s, and 16 or 32 workers on
the capped DLL had none in 4 runs each. Multi-second stalls of the reliable-data flood under the same
load showed up at every worker count (2 of 9 runs at 32, 3 of 9 at 4, 2 of 7 at 8, 1 of 4 at 16, 1 of
4 at 32 on the capped DLL), so they do not depend on the runtime size. Remote audio gaps over 30 ms stayed rare in every
configuration.

## Logging

With `capture_logs` set in `livekit_ffi_initialize`, records reach the host as `LogBatch` events,
filtered at the source by `LK_FFI_LOG_LEVEL` (`off`, `error`, `warn`, `info`, `debug` or `trace`,
case-insensitive; `warn` when unset or invalid). It is read on every `livekit_ffi_initialize` call
and sets three things: the logger's filter, `log::set_max_level` (so the `log` macros skip records
above it) and the minimum severity of libwebrtc's log sink (so libwebrtc does not format lines
that would be dropped). Without `capture_logs` the env_logger filter (`RUST_LOG`) sets all three.

libwebrtc records have the target `libwebrtc` and the message `(file.cc:line): text`:

| libwebrtc | Record level | Sink minimum severity for the level |
|---|---|---|
| `LS_ERROR` | `LogError` | `error` → `LS_ERROR` |
| `LS_WARNING` | `LogWarn` | `warn` and `info` → `LS_WARNING` |
| `LS_INFO` | `LogDebug` | `debug` → `LS_INFO` |
| `LS_VERBOSE` | `LogTrace` | `trace` → `LS_VERBOSE` |

`off` removes the sink. libwebrtc's info lines are many and mostly routine, so they need `debug`;
`info` adds the Rust SDK's info records only. In a 20 s two-room harness session (camera, screen
and mic published, camera republished once) the FFI forwarded 76,507 records before, 517 at the
default level and about 6,500 at `debug`.

libwebrtc's global minimum severity (`LogMessage::IsNoop`, which every `RTC_LOG` checks before it
builds a line) is the minimum over its debug severity, its `LoggingConfig`'s `min_severity` and
every sink's minimum (`UpdateMinLogSeverity` in m150's `rtc_base/logging.cc`). m150 creates the
`LoggingConfig` on the first call that reads it, adding a sink included, and its defaults are
`min_severity` and `debug_severity` `LS_INFO` and `log_to_stderr` on. Up to hw.18 the sink only
called `LogToDebug(LS_NONE)` and `SetLogToStderr(false)`, which change neither config field, so the
global minimum stayed `LS_INFO` whatever `LK_FFI_LOG_LEVEL` said, `off` included: every `LS_INFO`
line (about 300 a second in that session) was built, passed through `OutputToDebug` (its
`DefaultLogLine` string, and on macOS a `CFPreferencesGetAppBooleanValue` call per line) and handed
to the sinks under libwebrtc's global logging lock, device threads included, then dropped. Since
`rootapp/prod-cleanup` the sink's constructor first installs a `LoggingConfig` with both severities
`LS_NONE` and no stderr (`webrtc::InitializeLogging`, before anything else reads the config), so the
sink's minimum alone is the global minimum: at the default `warn` libwebrtc builds no `LS_INFO`
line, and with `off` none at all. What reaches the host is unchanged.
`cargo test -p webrtc-sys --lib webrtc::tests` checks the global minimum and which lines are
formatted and forwarded at each sink level (it fails against the hw.18 constructor).

The variable is read through the C runtime's environment. On Windows, .NET's
`Environment.SetEnvironmentVariable` before `livekit_ffi_initialize` is enough; on macOS and Linux
it only changes .NET's own copy, so set the variable before the process starts or through
`setenv`.

## Native call audio (`rootapp/native-platform-audio`)

Phases 0-3 of RootApp.Client.Desktop's `docs/native-platform-audio.md`: platform audio
(`PlatformAudio`, `AUDIO_SOURCE_PLATFORM`) carries a call, with device IO, mixing, echo cancellation,
volumes, effects, the mute/push-to-talk gate, (Phase 2) the app's voice chain and the Effects SDK
denoiser hand-off, and (Phase 3) the mic test monitor in native code.

`livekit_ffi_rootapp_audio_caps()` is a zero-protobuf export (every target) returning the native audio
features the build implements: bit 0 SELECT_DEVICES, 1 PLAYOUT_HOLD, 2 STATS, 3 ADM_HARDENED, 4 MIXER,
5 TRACK_GAINS, 6 EFFECTS, 7 CONTROL, 8 DSP, 9 DENOISER, 10 MONITOR, 11 APM_DECORATOR, 12
SPEECH_DETECTION, 13 SCREEN_AUDIO, 14 ECHO_MODEL. Screen audio plus the echo model
(hw.9 onwards) report `0b111111111111111` (32767: bits 0-14) on 64-bit Windows, macOS and Linux,
`0b111110111111111` (32255, no DENOISER) on 32-bit desktop and 0 on mobile; the echo-model branch alone
reported 24575 (no SCREEN_AUDIO), the screen-audio branch alone 16383 (no ECHO_MODEL); hw.8 reported 8191 (bits
0-12), builds up to hw.7 reported
4095 (no SPEECH_DETECTION), Phase 2 builds 3071 (no MONITOR) and Phase 1 builds `0b100011111111`
(2303). The
`RootAudio*` requests (`rootapp_audio.proto`, oneof cases from 1000) that a build does not implement
answer "not supported by this build"; an older library rejects them as an empty request, so hosts
check the export first.

What changes for every user of the library on Windows, macOS and Linux, the managed engine included:

- `AdmProxy` records the desired device state and one `audio-device` thread (COM MTA on Windows)
  applies it; the worker and FFI callers never wait for a device. `PlayoutDelay`, `Playing`,
  `Recording`, `*IsInitialized` and `BuiltIn*IsAvailable` read atomics. `PlayoutDelay` is 0 unless
  the platform device plays.
- Recording opens only on `StartRecording`, i.e. for a device track's send stream; stop-on-mute is
  off and `SetMicrophoneMute` is a no-op (on Windows it is the system-wide endpoint mute). Built-in
  AEC/AGC/NS (Windows' voice capture DMO) are off and refused.
- `PlatformAudio`'s own start/stop (`start_recording`, `AudioDeviceController::start_playout`, ...)
  are separate requests: a device runs while libwebrtc or the app asks for it, so neither switches
  off what the other started. `switch_*_device` is a selection change.
- The device selection is stored as {default | GUID | name} and applied before every open; default
  is the console role on Windows. A device that is not present fails the open (result -10000, a
  value no ADM returns) instead of falling back to index 0. libwebrtc's own engine-init selection
  (the communications role) is ignored.
- Playout runs while libwebrtc plays or the app holds it (`RootAudioSetPlayoutHold`).
- A device that fails to stop (the Windows ADM gives up joining its render or capture thread after
  2 s and leaks it) gets no second puller or recorder in that pass; the direction is retried a
  second later and the request reports the stop error.
- One render and one capture callback reach libwebrtc at a time, the synthetic pump included; one
  that starts while another is in flight (a leaked device thread) gets silence, as does a second
  concurrent `RootAudioMixer::Mix`.
- On desktop the device track uses software AEC and NS as asked, AGC off and the high-pass filter
  (mobile keeps its platform voice processing and AGC); external (`NativeAudioSource`) tracks set no
  processing options and never initialize the mic. From Phase 1 the factory's APM decorator also
  ignores a device track's `auto_gain_control=true` and `highpass_filter=false`: AGC1 and AGC2 stay
  off and the high-pass filter stays on for every desktop user.
- `RootAudioMixer` replaces `AudioMixerImpl`: fixed 48 kHz, no limiter, float mix, soft clip.
- libwebrtc's `Terminate` (m150: the voice engine's, with the last PeerConnection) only ends
  libwebrtc's own playout, recording and transport; both ADMs stay up until the factory goes, so the
  next PeerConnection gets audio again (see "The voice engine without a call").
- A transport tap counts the platform ADM's callbacks (`RootAudioStats`; `GetStats` only while
  platform audio is in use). In every build, locks taken and libwebrtc log lines raised (and
  dropped) on device threads are counted; `RootAudioStats` fields 70-73 report them with the
  overlapping callbacks and failed stops.

Phase 1 (desktop; mobile keeps libwebrtc's APM and reports no capability):

- `RootAudioMixer` plays each remote stream at `VolumeCurve(tile x master)` (v^2.5 capped at 10, the
  app's curve) with 20 ms linear ramps, 0 while deafened or paused, soft clips each stream and the
  sum, and mixes the effect bus after the remote streams, ignoring master and deafen. Gains are keyed
  by `Source::Ssrc()` and kept for streams that have not started. Once a gain was set, a stream that
  starts without one is held at 0 for up to 500 ms (its gain is on the way), then plays at tile 1;
  a forgotten SSRC leaves a stream that still plays at its last gain. While no `PlatformAudio` holds
  the ADM every gain is unity and the effect bus is idle, so managed calls are unaffected.
- Effects: `RegisterEffect` copies a 48 kHz mono int16 clip, kept for the factory's lifetime;
  `PlayEffect` queues it in a 32-slot ring (a full ring drops and counts it); `StopEffects` cuts
  playing and queued effects, and so does the last `PlatformAudio` reference going away. An effect
  that waited over 1 s for the render thread (a stalled or still-opening device) is dropped and
  counted instead of played late.
- The factory's APM is built here: libwebrtc's builtin APM with `RootCaptureProcessor` as its
  capture post-processor, wrapped in `RootApmDecorator`. The decorator forwards every
  `AudioProcessing` call; its `ApplyConfig` keeps AGC1 and AGC2 off and the high-pass filter on and
  takes AEC and NS from `RootAudioSetControl` once set (applied from the worker thread, at most once
  a second; from Phase 2 also at a pause in speech), and it swallows `set_output_will_be_muted` and the
  `kCaptureOutputUsed` setting, so the post-processor runs on every frame. `GetConfig` serves the
  voice engine's own request with only AGC off and the high-pass filter on, so the engine's
  read-modify-write never copies the AEC/NS policy into it.
- `RootCaptureProcessor` gates the mic (muted or push-to-talk key up), with a 5 ms fade each way and
  both gate fields in one atomic word, applies the input volume with a 20 ms ramp and counts levels,
  at whatever rate and channel count the APM hands it (8 kHz streams are processed at 16 kHz). Its
  format reaches the host as `RootAudioEvent.capture_format` from a task that reads atomics every
  10 ms, starting 100 ms after the handle is created (`RootAudioStats.process_rate` has it too).
- Track gains (`RootAudioSetTrackGains`) are keyed by the remote track's FFI handle and mapped to the
  stream's SSRC: the receiver's signaled SSRC (the receive stream's `remote_ssrc`), else the track's
  inbound-rtp SSRC. While a session is live a remote audio track's SSRC is resolved as it is
  subscribed, so the first gain applies inside its request; otherwise off the caller's thread, one
  resolution per track at a time. Unknown SSRCs are resolved again every second, known ones every
  5 s and 50 ms after the mixer gets a new stream (a renegotiated SSRC). Gains of dropped track
  handles are forgotten; handles that are not remote audio tracks come back in
  `unknown_track_handles`, not as an error.
- The `PlatformAudio` handles of a process share one engine. Its controls go back to their defaults
  (gate open) when the first handle's session starts; when the last one is dropped the gate closes
  (muted, push-to-talk key up) and gains, effects and the AEC/NS policy reset, before the handle
  lets go of the device. Dropping one of several handles changes nothing.
- `RootAudioSetControl` fields `denoise_power`, `auto_gain`, `gate_threshold`, `auto_sensitivity`
  and `gain_reset_db` are stored for the DSP chain (Phase 2 acts on them). A field the build cannot
  act on (no capture processor or decorator, e.g. when the APM failed to build) is answered "not
  supported by this build"; the other fields are applied.

`echo_cancellation_active` and `webrtc_ns_active` report the decorator's applied config; ERL, ERLE,
the echo delay and the divergent filter fraction come from the APM's statistics (NaN while it has
none, the delay -1). Reading them takes the APM's statistics lock, which the capture thread holds
briefly once per frame (as libwebrtc's own getStats does); the rest of `RootAudioStats` is atomics.

### Phase 2: voice chain, Effects SDK hand-off, speech-pause APM policy

Caps: DSP (bit 8) on every desktop build, where the factory builds its APM (`kRootDspSupported`, the
condition of `LK_ADM_MOBILE`); DENOISER (bit 9) on 64-bit desktop only, where the Effects SDK's
vtable calls follow the x64/arm64 C convention (`kRootDenoiserSupported`). The test
`rootapp_audio_caps_cover_phase_2` ties both bits to those native constants and DENOISER to a
non-null `livekit_ffi_rootapp_denoiser_notification_handler()`; `RootAudioStats` carries the DSP
fields (74-83) only where `dsp_available` (the same constant, and an APM that was built).

What changes for every desktop user whose device track goes through the factory's APM (not the
managed engine's `NativeAudioSource` tracks, which bypass it):

- `RootCaptureProcessor` runs `rootapp-voice-dsp` (pure Rust, no dependencies, no allocation, lock,
  I/O or panic after construction) on channel 0 and copies the result to the other channels:
  sanitise -> denoiser slot -> frame aligner -> voice gain -> auto sensitivity -> startup hold ->
  noise gate -> input volume (20 ms ramp) -> mute/push-to-talk transmit gate (5 ms fade). The
  input-volume ramp and the gate fade moved here from Phase 1's C++. Managed constants are used
  unchanged (the crate scales FloatS16 by 2^-15 on entry); other rates scale the gate block by time
  (128 samples at 48 kHz, 43 at 16 kHz).
- With no `RootAudioSetControl` the chain runs on its defaults: noise gate in auto sensitivity
  (threshold from the noise floor of the denoised voice before the voice gain, 8 dB over it but never
  below -54 dBFS (-60 before hw.11; see "Auto sensitivity"), plus the frame's voice gain; 5 blocks to
  open), the chain's voice gain on from 0 dB (it only boosts, up to 24 dB; 15 in hw.12, 18 before;
  see "Voice gain" and "Voice gain: AGC2"; since `rootapp/prod-cleanup` a session starts with AGC2
  and the speech gate, see "Production cleanup"), no denoiser. Every recording start silences its first 20 frames (200 ms, the startup
  hold), and with `denoiser_wanted` up to 100 frames until the denoiser runs. Mute and push-to-talk
  key up silence after the 5 ms fade; the gate, voice gain and auto sensitivity keep running.
- Speaking (`RootAudioEvent.speaking`, stats 74/42) is the noise gate's speaking (envelope plus a
  300 ms hold) while the transmit gate is open, so it is never reported while muted or with the key
  up. It clears on `reset_root_audio` and when recording stops (no frame would clear it).
- A recording start bumps the capture state's stream epoch (AdmProxy, right before
  `StartRecording`): the aligner, the denoiser (flush) and the startup hold start over. A rate change
  does the same; a session start (first handle) resets everything, the voice gain to 0 dB.
- The APM decorator applies host AEC/NS changes at most once a second and at a pause in the user's
  speech (published speaking false and not in the startup hold), at most 3 s late (then counted as
  forced). Native WebRTC NS covers a wanted denoiser that stopped: on at once for an output stall
  or slow frames (an urgent apply on the worker thread, no rate limit or pause), after 1 s of
  flowing frames when it is missing, removed or at the wrong rate; off again with the normal rules.
  Policy stats are atomics and never wait for an APM re-initialisation.
- 64-bit desktop: `RootDenoiserSlot` runs an app-created `IPipeline*` on the capture thread (slots
  4, 8 and 9 only; native never calls `release`). The `rootapp-effects` thread starts on the first
  `RootAudioSetDenoiser` or `denoiser_wanted` and installs at a speech pause (<= 3 s), removes at
  once, and hands each replaced pipeline back once (`pipelines_returned`). Bypasses: rate mismatch
  (clears itself when the rate is back), 100 empty outputs in a row, or slow frames (3 over 5 ms
  within 60 s, or one over 20 ms). Flush, power change and `process()` are timed together into a
  16-bucket histogram and a maximum. The first call after an install or a flush is exempt from the
  single 20 ms latch (still measured, still counted toward the 3-in-60 s rule): the Effects SDK
  1.7.2 harness measured no first-call spike (2,649 calls from a fresh pipeline, maximum 516-525 us
  including call 1), so the exemption only matters for a one-off warm-up on a slower machine, which
  would otherwise turn into a final, process-wide Effects SDK disable on the desktop.
- `RootAudioReturnPipelines` waits up to 200 ms in all (a change in progress and the audio thread);
  with a scheduler tick and two short control-mutex waits the desktop's exit wait is sized at 300 ms.
  The last session's reset runs outside the sessions lock; what it gives back waits for the next
  handle or a `RootAudioReturnPipelines` response.
- `livekit_ffi_rootapp_denoiser_notification_handler()` (new export, null on mobile and 32-bit) is a
  static `INotificationHandler` that only counts notifications (stats 57 and 88).
- Stats 34-45, 50-57 and 74-100 are filled. Field 89 (`denoiser_latency_ms`) is the latency the
  aligner adds now, since it last started over; the managed figure counted from the denoiser's
  creation, grew with every aligner reset and fell back after trims, so cumulative aligner counters
  (90-94) are what a caller baselines per call. 38 and 40 are maxima since the previous read.

Limits and what is not verified:

- macOS and Linux were not compiled for this phase (Windows cannot build webrtc-sys for them; see
  the checklist). The changed C++ has no platform-specific code; the mobile branches
  (`WEBRTC_IOS`) pass a clang-cl syntax-only check, and MSVC and clang-cl `/W4` add no warnings.
- The real Effects SDK was not driven natively: `createPipeline` at 8/16/32/44.1 kHz and
  `pcmScale 32768`, `process()` latency on the capture thread (E8) and the slow-bypass thresholds
  are unmeasured. Fakes with the SDK's vtable layout and output pattern stand in for it.
- 8 kHz device streams reach the processor at 16 kHz (the APM upsamples); the chain's 8 kHz path
  runs only in tests and golden vectors.
- A device that dies mid-word without a stop keeps speaking published until the next stop or reset.
- The golden vectors match bit for bit on Windows x64; other math libraries may need
  `ROOTAPP_DSP_GOLDEN_LOOSE=1`.

Known limit (libwebrtc prebuilt): `external_audio_source.patch` as built into the pinned prebuilt
never clears `sending_` in `Stop()`, and `StoreEncoderProperties` still registers a sending external
stream with AudioState when its send codec changes (format, payload type or RED). That stream would
then get the mic's audio, and after unpublish AudioState would keep a pointer to the deleted
stream. The patch in this repo fixes both, but only a libwebrtc built from it has the fix. Until
then: a published audio track's codec options (RED, DTX, stereo) never change in place (they are
fixed at publish; stereo munging is keyed to the track id in every offer), and `RtpSender` refuses
`set_track` and inactive encodings on audio senders (unpublish and republish instead).

The server's answers change codecs too: LiveKit 1.13.7 with a single peer connection answers a new
stereo screen-audio section `minptime=10;stereo=1;useinbandfec=1` and every later negotiation (the
screen video published right after it, any track change) `maxaveragebitrate=510000;...`, because it
resolves the track only from the second answer on. That format change while sending registers the
screen-audio stream with AudioState on the platform engine (the live 10/10 symptom: 100 packets/s
at ~256 kbps against a 128 kbps cap, mic frames interleaved with the screen audio, garbled at the
receiver). The publisher transport keeps the payload order and Opus fmtp of every audio send section
whose track has an external source (`RtcAudioSource::Native`: screen-share audio, which the native
capture attaches to a NATIVE source, and the managed engine's mic) as its first answer gave
them (`PeerTransport::pin_send_codecs`, a stereo track's first answer gets `stereo=1` if the server
left it out) and logs the first change it overrides per section. `create_sender` (`rtc_session.rs`)
registers each such track's id with the transport (`add_external_audio_track`, next to
`add_stereo_track`; neither set is cleared on unpublish: a released section no longer carries the
track's msid, so its pin goes, and a republished track keeps its id and its source).

A device track (`RtcAudioSource::Device`, the platform engine's mic) is not pinned (hw.10): its send
stream is fed through AudioState anyway, so the codec-change bug never reaches it, and its section
takes every answer as the server gives it, payload order, RED, DTX and stereo included. Pinned
(hw.6-hw.9), the mic kept a first answer in which the server, not yet resolving the section, echoed
the single-PC offer's `stereo=1` munge without `usedtx=1`; the later answers asked for DTX, but the
mic sent stereo music-mode Opus with DTX off for the whole call, about 50 packets/s while silent
instead of about 5. The first-answer `stereo=1` fix-up is part of the pin, so it does not reach a
device track either (the desktop publishes only screen-share audio with `force_stereo`). The managed
engine's mic is a NATIVE source and keeps the pin.

Device-free tests: `cargo test -p livekit --lib peer_transport` (string cases: an external section
pinned through a DTX and RED change, a device section never pinned, both in one answer; plus a
loopback pair whose answer gains `maxaveragebitrate`: a registered external track's send codec stays
and the same answer applied past the transport changes it, an unregistered track (standing in for a
device track, which needs the platform ADM) follows the answer through the transport). Against a
local server (`livekit-server --dev --bind 127.0.0.1`): `cargo test -p livekit --features
__lk-e2e-test --test screen_audio_codec_test -- --nocapture` (fails without the pin; its tracks are
all NATIVE sources).

The same answers put one Opus payload type with different fmtp in several BUNDLEd sections. m150's
`SdpOfferAnswerHandler` (`pc/sdp_offer_answer.cc`) rejects such a payload type collision in
`SetRemoteDescription` only when the field trial `WebRTC-SdpBundlePayloadTypeCollisionCheck` is
enabled and only logs it otherwise; the trial is off by default today. Every factory's environment
now carries `WebRTC-SdpBundlePayloadTypeCollisionCheck/Disabled/` (`RootAppFieldTrials` in
`webrtc-sys/src/peer_connection_factory.cpp`, composed with the zero-playout-delay and WARP trials),
so a libwebrtc that turns the check on by default cannot fail every LiveKit answer. Test:
`cargo test -p libwebrtc --lib peer_connection_factory` (the default factory and one with both
options both answer `Disabled` for the trial and keep their other trials).

Tests that need no device: `cargo test -p webrtc-sys --lib -- adm_proxy root_
device_selection` (AdmProxy over a fake platform ADM that records its calls, the mixer, the capture
processor, the decorator over a fake and over the real builtin APM, the denoiser slot and its
control side, and `root_voice_*`: the whole voice path over one capture state with fake Effects SDK
pipelines and a worker thread), `cargo test -p rootapp-voice-dsp` (ported managed tests, 25 golden
vectors from the desktop's reference, no-allocation and no-panic sweeps; `cargo clippy -p
rootapp-voice-dsp --all-targets -- -D warnings`), `cargo test -p libwebrtc --lib imp::rtp_sender`,
`cargo test -p livekit platform_audio`, and `cargo test -p livekit-ffi --lib rootapp_audio` and
`platform_audio`. Run them with `--release`. The harnesses are behind webrtc-sys's `harness`
feature, which the dev-dependencies turn on for these commands (see "Production cleanup"); a
`cargo build` of the library never has them. Every test that creates the platform ADM
(`test_audio_device_controller_bridge`, `test_new_platform_audio`, the device tests in
`livekit/tests/platform_audio_test.rs`) runs only with `LK_HW_AUDIO_TESTS=1`.

### Phase 3: mic test monitor, macOS and Linux device ids

Caps: MONITOR (bit 10) wherever DSP is (the monitor is fed by the capture processor);
`rootapp_audio_caps_cover_phase_3` ties it to `root_dsp_supported()`.

Mic test monitor (`RootAudioSetMonitor`, `PlatformAudio::set_monitor`, stats 102-104):

- `RootAudioMonitor` plays channel 0 of `RootCaptureProcessor`'s output (what a call would send:
  gate, mute, push-to-talk, voice gain, input volume and the startup hold applied) at unity gain on
  every playout channel. The transport tap adds it to device render callbacks after libwebrtc's
  `NeedMorePlayData`, which has already given its mix to the APM as the AEC reference, so the echo
  canceller never sees it; the synthetic pump never plays it.
- One producer (capture post-processor) and one consumer (render callback) share 16 preallocated
  10 ms frames. The consumer owns a prime target: playback starts once 30 ms are queued (older
  frames skipped), each underrun adds 10 ms (up to 80 ms, for devices whose callbacks come in
  bursts, e.g. PipeWire quanta) and 10 s without one takes 10 ms away. Latency that stayed more than
  20 ms above the target for 100 callbacks is skipped; a render callback after more than 6 capture
  frames without one (a stall) drops what queued and primes again; `AdmProxy` resyncs the ring
  before every device stop and start, so frames captured across a restart never play. A capture
  frame that finds the ring full counts only while the render side runs. 2.5 ms fades after
  priming and around underruns and skips. `monitor_frames` counts render frames played in full,
  `monitor_dropped_frames` underruns, skipped frames and full-ring drops while rendering.
  Catmull-Rom rate conversion on an integer phase converts the processor's 16/32/48 kHz to any
  render rate without allocation; equal rates pass samples through with the APM's own int16
  rounding (bit-exact with what the APM sends, outside the fades).
- While libwebrtc records for no send stream (the mic test, the app's own start), the transport tap
  averages the device's channels to one before libwebrtc (as `DownmixInterleavedToMono` does for a
  mono send stream), so the APM and the processor see what a call sends; a send stream's
  `StartRecording` hands libwebrtc the device's channels again.
- `AdmProxy::SetMonitor` adds a want of its own on both directions (OR-ed in like the hold): the
  current selection opens on the device thread, `Playing()`/`Recording()` stay libwebrtc's. It is
  refused while libwebrtc records for a send stream, and libwebrtc's `StartRecording` (a call that
  publishes the mic) turns it off, keeping the recording device open for the call. The last
  reference and the last session's reset (`reset_root_audio(true)`) turn it off.
- Verified in `audio/audio_transport_impl.cc` (m150): with no send stream every capture frame still
  goes through the APM, at the device rate rounded up to 8/16/32/48 kHz with the device's channels,
  and is dropped after processing. The post-processor runs at 16/32/48 kHz.

Device ids (all platforms; the macOS and Linux rules come from `audio_device_mac.cc` and
`audio_device_pulse_linux.cc` at m150):

- GUIDs match exactly: Windows MMDevice ids; macOS AudioObjectIDs as decimal strings, index 0 being
  the current default under the GUID "default" and the default listed again under its own id.
  AudioObjectIDs move after a replug or a coreaudiod restart (when a stored one can name another
  device), so macOS selects by UID (`RootAudioDeviceSelection.uid`): `ResolveSelectionLocked`
  translates it with `kAudioHardwarePropertyTranslateUIDToDevice` at every open and matches the id;
  an unknown UID is DeviceNotFound, never another device, and a UID selects nothing elsewhere.
- Names match exactly (Linux: the PulseAudio description; GUIDs are empty there), or by the complete
  UTF-8 characters an ADM kept of a name it cut to 127 bytes, so the full name and the FFI's lossy
  copy both match. Of duplicate names the first in the ADM's order wins.
- ADM name buffers are read up to their size and handed to the ADM with a zeroed byte past it (the
  macOS ADM copies a CoreAudio name unterminated when it fills the buffer, and libwebrtc's ADM
  wrapper logs it as a C string), and names and GUIDs cross the cxx bridge lossily:
  `rust::String`'s constructor throws on a name cut inside a UTF-8 sequence, which aborted the
  process.
- Linux: the PulseAudio ADM enumerates through members its `InitPlayout`/`StartPlayout` also use
  (`_deviceIndex`, `_numPlayDevices`, the display-name pointers) with no lock of its own, so
  enumeration from the FFI takes the device mutex there, waiting at most 2 s for a device operation
  (a PulseAudio stream start can wait 12 s per direction); past that it reports no devices, and
  the enumerations in the next 2 s give up at once. Windows and macOS enumeration stays lock-free
  (their ADMs enumerate safely on any thread).

Portability review of the fork's Phases 0-3 C++ and Rust:

- Platform branches are few: COM and `kDefaultDevice` (Windows), the enumeration lock (Linux), and
  the mobile branches. Each file includes the standard headers it uses (checked by hand, since
  libc++ includes less transitively than MSVC's STL). No MSVC-only constructs; `std::atomic<double>` and (in the monitor)
  `std::atomic<uint64_t>` lock-freedom is asserted (true on every 64-bit desktop target; it would
  fail a 32-bit ARM build).
- The Effects SDK is called through raw vtables (no virtual destructors, `this` first); that holds
  for the Itanium ABI on x86-64 SysV and AArch64 as for MSVC, as the desktop's managed binding
  already relies on.
- Upstream, not fixable here: `AudioDeviceMac::GetDeviceName` formats the default device's name
  with `%s` from a 128-byte array CoreAudio may fill without a terminator when the name is that long
  (a stack over-read when the default device's name has 128 bytes or more; E7 with such a device);
  AUHAL's stream-format listener reconfigures the render/capture
  converters on a HAL thread without locking against the IO threads (AirPods profile switches); the
  PulseAudio ADM's `RTC_DCHECK(thread_checker_)` calls would fire from the device thread if a
  prebuilt were built with DCHECKs on.
- Checked here: every audio C++ file compiles syntax-only with clang-cl (`/W4`) under the Linux
  (`WEBRTC_POSIX`, `WEBRTC_LINUX`) and macOS (`WEBRTC_POSIX`, `WEBRTC_MAC`) defines against the
  Windows prebuilt's headers, MSVC's STL and stubbed POSIX headers (and, for the UID translation,
  stubbed CoreAudio/CoreFoundation declarations with the SDK's types); no warning in our files
  except the existing unused `AdmProxy::worker_thread_`. Not checked here: AppleClang with Apple's libc++,
  clang with the hermetic libc++ and a Linux sysroot, linking, and running anything on macOS or Linux.

Still for CI (macOS x64/arm64, Linux x64/arm64): build and link; the device-free suites above plus
`root_monitor_`; `rootapp-voice-dsp`'s golden vectors (other libm builds may need
`ROOTAPP_DSP_GOLDEN_LOOSE=1`); `NDEBUG` without `DCHECK_ALWAYS_ON` in the Linux and macOS
prebuilts' `webrtc.ninja`. Live (desktop gate E7): the monitor on each OS, an AirPods profile switch
mid-call, PulseAudio and PipeWire-pulse descriptions, duplicate device names.

Tests that need no device: as Phase 2, with `root_monitor` (the ring, priming, underrun and overrun
counting, 16->48, 48->44.1, 32->48 and 48->16 kHz against an analytic sine, saturation),
`adm_proxy_monitor_*` (devices opened without claiming them, stop leaves what others keep, refused
while sending, off when sending starts, needs a reference, follows a selection, teardown),
`root_voice_monitor_*` (through the real APM: the device plays exactly what the APM sends while the
APM's reverse stream holds only the remote mix; 16 kHz capture to 48 kHz playout; a call takes over)
and `device_selection_*` (macOS- and Linux-shaped device lists, cut names).

### First live test fixes (2026-10-10)

- Stereo playout: libwebrtc's `adm_helpers::Init` (engine init, m150 `media/engine/adm_helpers.cc`)
  calls `InitSpeaker` and `SetStereoPlayout(StereoPlayoutIsAvailable())` once. Under the playout
  hold the device is already open then, so `AudioDeviceModuleImpl::SetStereoPlayout` refused
  ("unable to set stereo mode while playing side is initialized") and the Windows ADM's
  `InitSpeaker` returned -1 while playing ("Unable to access speaker"; it only refreshes the
  endpoint and takes the session's `ISimpleAudioVolume`, it sets no volume). Windows still played
  stereo (its ADM prefers 2 channels by default and sets the buffer's channels at every
  `InitPlayout`), but PulseAudio starts mono (`_playChannels(1)`) and macOS stays mono after a mono
  device, so remote stereo, screen-share audio and the music bot could play mono there. On desktop
  `AdmProxy` now keeps libwebrtc's choice (stereo until it says otherwise) and applies it before
  every playout open; `InitSpeaker`/`InitMicrophone` are no-ops (every open initializes them), and
  `SetSpeakerVolume`/`SetSpeakerMute` refuse without reaching the ADM (no m150 caller in
  `media/`, `audio/`, `pc/` or `api/`; the app's volume is the mixer's).
- Per-source playout levels (`RootAudioStats` 105-106): the mixer counts each source's audio before
  its gain per slot (a muted frame as silence); `livekit_ffi` names each source's remote track handles
  from the SSRCs it resolves for track gains and at subscribe. The app shows each person's audio and
  checks their ring against it on platform calls.
- The denoiser's slow latch was left as is: in the live test one 1.7 s system burst (capture
  callbacks bunched too) latched the pipeline and, 0.2 s after its install, the replacement with
  genuine 5-10 ms calls; the third pipeline ran at 1-1.5 ms at once and the first install's first
  calls took 1 ms, so the one-call warm-up exemption holds. The app counts such a burst once.

Tests: `adm_proxy_playout_opens_stereo_*`, `adm_proxy_never_touches_the_speaker_*`,
`adm_proxy_stats_count_mixer_source_levels`, `root_audio_mixer_counts_*`,
`root_audio_mixer_source_added_again_*`, `root_audio_stats_carry_per_source_levels_*`,
`rootapp_audio_source_levels_*`.

### Noise at join (2026-10-12, hw.6)

- The first platform call of a session played loud noise from the lease until libwebrtc registered
  its transport about 0.65 s later. m150 initializes the voice engine (`adm_helpers::Init`, then
  `RegisterAudioCallback`) with the first PeerConnection (`ConnectionContext::AddRefMediaEngine`),
  after SessionCreate and the room connect; the lease's playout hold had opened the device first.
  `AdmProxy` registered its tap with the platform ADM only with libwebrtc's transport, so the
  device pulled an `AudioDeviceBuffer` without one: `RequestPlayoutData` resizes its playout buffer
  (`new int16_t[]`, uninitialized) and returns 0, and `GetPlayoutData` copies that buffer to the
  device every 10 ms ("Invalid audio transport", "nSamples(0) != _playBlockSize480", then "Setting
  audio transport while media is active"). The mixer's meter never saw it. Later calls in the
  session had the transport already. The Settings mic test (the monitor) opened the device the same
  way.
- The tap is now registered with the platform ADM when the ADM is created (and on Android when it is
  created on the first acquire) and stays until shutdown; it plays silence while libwebrtc has no
  transport. Not caused by the stereo change (8ea2a3b8 is inert on Windows, whose ADM already
  prefers stereo); by its voice trace the 10-10 run's window was at most about 0.1 s.
- The harness's fake ADM now plays through libwebrtc's own `AudioDeviceBuffer` as the Windows render
  thread does (`device_render`, `stale_device_buffer`), and its transport can play the mixer.

Tests: `adm_proxy_device_plays_silence_until_libwebrtc_has_a_transport` (hold, no transport, a
full-scale stale buffer: silence; then the chime through the mixer: the device plays exactly the
mixer's metered output), `adm_proxy_mic_test_device_plays_silence_without_libwebrtcs_transport`.

### The voice engine without a call (2026-10-12, after hw.6)

- m150 initializes the voice engine (`WebRtcVoiceEngine::Init`: `adm_helpers::Init`, the APM options,
  then `RegisterAudioCallback` with AudioState's `AudioTransportImpl`, which runs the APM and so the
  capture processor and the monitor on capture, and the playout mixer on render) only when a
  PeerConnection takes its reference to the `ConnectionContext` (`PeerConnection::Initialize`,
  `ConnectionContext::AddRefMediaEngine`), and terminates it when the last one drops it
  (`~PeerConnection`, `ReleaseMediaEngine`, `WebRtcVoiceEngine::Terminate`). Without a PeerConnection
  the Settings mic test's capture never reached the APM (the monitor played the tap's silence and the
  desktop fell back to the managed test after 3 s), and the hold played no mixer (no effects) before
  the first PeerConnection or after the last.
- `WebRtcVoiceEngine::Terminate` ends with `adm->Terminate()`, which shut the proxy and both ADMs
  down for good; the next PeerConnection's engine init only calls `Init` (a no-op), so a factory that
  outlived its last PeerConnection had no audio from then on: no device opened for a platform call
  or the mic test, and no synthetic pump for a managed call (remote audio stops reaching its sinks).
  A factory outlives its PeerConnections while anything holds the runtime, e.g. a call's lease kept
  past the room's end (the leave chime, the desktop's up to 5 s), a call joined within that time, or
  the mic test on the same runtime.
- The factory now builds its `PeerConnectionFactory` as `CreateModularPeerConnectionFactory` does
  (`ConnectionContext::Create`, `PeerConnectionFactory`, `PeerConnectionFactoryProxy`) but keeps the
  context. `RootVoiceEngineHold` takes a `ConnectionContext::MediaEngineReference` of its own, as a
  PeerConnection does, on the worker thread while a platform reference is held: every
  `AudioDeviceController::acquire_platform_adm` posts one up, every release one down (never waiting),
  and `~PeerConnectionFactory` shuts it down before letting go of the context. Desktop only; mobile
  takes no reference.
- `AdmProxy::Terminate` now only ends what libwebrtc itself asked for (its playout, recording and
  transport); the hold, the app and the monitor keep theirs, a held device plays the tap's silence
  (never a stale buffer), and only the destructor shuts the ADMs down. The next engine init, from a
  PeerConnection or a platform reference, finds the proxy working.
- Consequences: the voice engine initializes when the first `PlatformAudio` handle is created, so
  effects (the join chime) play as soon as the lease's hold opens the device, before the room
  connects, and after the last PeerConnection while the lease is held; the mic test hears what a call
  would send with no call ever made. The managed engine (no platform reference) takes no engine
  reference, opens no platform device, and gets its synthetic pump again in every call.

Tests (`root_engine_*`, `RootEngineHarness`: libwebrtc's own `WebRtcVoiceEngine` in a
`ConnectionContext` over `AdmProxy` and the fake platform ADM, wired as the factory wires it, with the
hold; PeerConnections are stood in for by their `MediaEngineReference`s, none is created):
`root_engine_mic_test_plays_processed_capture_before_any_call` (every frame through the APM and the
processor, every render through libwebrtc's transport and the mixer, the device plays exactly what a
call would send; muted, silence), `root_engine_mic_test_works_again_after_the_engine_terminated`,
`root_engine_a_call_ending_under_a_lease_leaves_the_device_working` (join chime before the
PeerConnection, leave chime after the last, the next call plays and captures),
`root_engine_mic_test_runs_on_across_a_call_ending`,
`root_engine_managed_calls_never_open_the_device_and_each_gets_the_pump`;
`adm_proxy_engine_terminate_under_the_hold_plays_silence_until_the_next_init`,
`adm_proxy_shutdown_stops_the_devices_before_clearing_the_callback`. Without the hold the first four
fail (nothing processed, no mix); with the old `Terminate` the second, the fifth and the
`adm_proxy_engine_terminate_*` test fail. The `adm_proxy_device_plays_silence_*` and
`adm_proxy_mic_test_device_plays_silence_*` tests still cover the time before the posted engine init.

### Speech detection (2026-10-12, lingering ring step 2)

The noise gate alone kept the speaking ring on 0.7 s after speech at best, and for seconds on breath,
keys and room noise above its threshold (desktop `docs/voice-live-test.md`, the 10/12 trace). Design
and evaluation: the desktop's `docs/native-platform-audio.md` ("Speech detection").

- `RootSpeechDetector` (webrtc-sys, `root_speech_detector.{h,cpp}`) runs libwebrtc's AGC2 speech
  detector (`agc2/rnn_vad`, already linked into the library) as `VoiceActivityDetectorWrapper`
  does, without its 1.5 s periodic reset: a `PushResampler` per APM rate (16, 32, 48 kHz) to 24 kHz,
  the feature extractor and the RNN; any other rate or length gives -1. Instead of the periodic
  reset, the RNN starts over after 100 frames in a row at p >= 0.3 (flat white hiss could hold it at
  speech). AVX2 is used only with FMA3 (rnn_vad's AVX2 code is built with FMA). Built in
  `RootCaptureProcessor`'s constructor (about 6 KB plus 44 KB of heap); `Analyze` and `Reset` never
  allocate, lock or log. It resets on a DSP reset, flush or rate change, and when it was not running
  on the previous frame; a reset runs a frame of zeros through each resampler used since the last
  one. Its header is included from .cpp files only (the rnn_vad headers raise C4244/C4267 at /W4;
  the includes are wrapped in `#pragma warning`).
- `RootCaptureProcessor` runs it on the frame `pre_denoise` sanitised, before the denoiser, while
  `speech_detection` is not off (also while muted or with push-to-talk, so it is warm when they end),
  and hands the probability to the chain (`RootDspSettings.speech_probability`).
- `rootapp-voice-dsp`: `SpeechGate` (`speech_gate.rs`) and `NoiseGate::process_controlled`. Speech
  is confirmed after 3 frames in a row at p >= 0.5 and ends 300 ms after the last frame at p >= 0.3.
  The ring (`speaking`) follows what is sent (hw.15): any frame that sends audio, plus the level gate's
  300 ms hold, while the transmit gate is open and outside the startup hold (unsupported frames: whatever
  goes out). Until hw.14 it was the level gate's speaking AND confirmed speech, so audio the detector did
  not confirm went out without a ring. In `Gate` mode (hw.17, strict: Jesse 10/16) only confirmed
  speech goes out:
  - the gate may open only while speech is confirmed (`GateControl::allow_open`), and opens on the
    confirming frame's first block above the threshold while the level gate alone is open
    (`eager_open`). Keys, clicks, a trackpad, breath, noise and speech the detector rates under 0.5
    never go out; the first frames of a word wait for the confirmation. Measured on real captures
    (Effects SDK at 0.60): 4-12% of sentence starts lose 20 ms or more, p90 0-20 ms; 0.3-0.4% of later
    speech; laughter 99% kept. Until hw.16 the level opened provisionally (up to 200 ms, longer while
    frames stayed at 0.3 or more), which sent every keystroke and uncertain speech;
  - when confirmed speech ends (300 ms below 0.3) with the gate open, it is released over 50 ms
    without the speaking hold and opens again only on newly confirmed speech. A verdict that starts
    over (a flush, a mode change) closes nothing: an open gate keeps its level release;
  - at or below -66 dBFS a frame opens nothing whatever the threshold;
  - auto sensitivity follows the level gate alone (`NoiseGate::level_envelope`), so the threshold is
    Off's and Gate never applies more gain than Off at the same frame;
  - since hw.18 the gate decides on each frame and applies the decision to the audio 40 ms older, so
    a word the detector confirms within 40 ms goes out from its first frame (see "Speech gate
    look-ahead").
  Push-to-talk, frames without a probability (off, other rates, NaN or negative) and unsupported
  frames leave the chain bit for bit as the level gate; the verdict keeps running under push-to-talk,
  so speech that continues when the key mode ends is already confirmed. Mute does not bypass the speech
  gate (nothing is sent while muted anyway). A change of mode starts the verdict over.
- Control `RootAudioControl.speech_detection` (`RootAudioSpeechDetection` OFF/RING/GATE; unknown
  values off), stored in `RootCaptureState::speech_detection`, back to off when the first handle
  opens and when the last closes (`reset_root_audio`, as the other DSP settings). Since
  `rootapp/prod-cleanup` RING is gone (the wire value is applied as GATE) and a session starts with
  GATE; see "Production cleanup".
  Stats 107-113: the mode, the highest probability since the previous read (-1 none), whether the
  speech gate judged the last frame and confirmed speech on it, and frames with confirmed speech,
  closed by the speech gate, and with detection on but no probability. Caps bit 12 SPEECH_DETECTION
  wherever DSP is (`rootapp_audio_caps_cover_phase_3` checks 8191 and ties the bit to `root_dsp_supported()`).

Tests: `cargo test -p rootapp-voice-dsp` (`tests/speech_gate.rs`: the state machine, confirmation,
hang, the forced close and re-arm, the bypasses bit for bit; Gate never sends a sample louder than
Off over random scripts with auto and manual thresholds; auto sensitivity in Gate is Off's; words
where the hang runs out, after soft tails, quiet and noise-filled gaps and held breath; a click or
flicker never re-arms; a mode change and a threshold of 0; `no_alloc` and `no_panic` vary the mode,
the input level and NaN, inf, negative and > 1 probabilities; the golden vectors also hold in Ring
mode and in Gate mode with constant speech; the bench runs with detection off and on); webrtc-sys
`speech_tests` (`root_speech_detector_matches_libwebrtcs_wrapper_at_16_32_and_48_khz`: identical to
the wrapper on synthetic signals until its first latch reset; decisions on a vowel, a tone, white
noise, silence, breath, clicks and keys; 10 min of white hiss at -60 dBFS under 25% at p >= 0.5
(75% without the latch reset); a rate change drops the resamplers' history; the capture processor
in Gate mode never rings on breath, clicks or keys, rings 30 ms into a voice and stops 300 ms after
it, Ring mode sends what Off sends, 8 and 44.1 kHz pass the level gate bit for bit, push-to-talk
bypasses it and the detector keeps judging while muted, a reset turns it off, the real APM runs it
at 16, 32 and 48 kHz), `adm_proxy_controller_stores_speech_detection_*`,
`root_voice_speech_detection_keeps_keys_off_the_ring_and_rings_for_a_voice`, and the livekit and
livekit-ffi conversions.

Review fixes (2026-10-12; the desktop's `docs/native-platform-audio.md` has the replays): Gate no
longer lowers the threshold for confirmed speech (it sent background speech Off never sends) and no
longer mutes a word that starts where the hang runs out; held word starts lose 70/30 ms max/median
after a breath (was 170/50) and none of the speech energy after noise-filled pauses (was up to 80 ms).
Auto sensitivity's minimum rises with at most 11 dB of the voice gain (`voice_gain_cap_db`; desktop
e435c4dc2, golden vectors regenerated: 25 vectors, two new at the cap). Powers of 10 go through
`pow10` (libm's pow with an opaque base), so LLVM cannot turn them into exp10 on macOS and Linux;
golden vectors written on Windows may still need `ROOTAPP_DSP_GOLDEN_LOOSE=1` on another libm.

## Native screen-share audio (`rootapp/native-screen-audio`)

Screen-share audio was the last real-time audio path in .NET (OS loopback, 100 ms batches, a managed
pacer). It now runs natively; design: the desktop's `docs/native-platform-audio.md` ("Screen-share
audio").

- **Backend interface** `webrtc-sys/include/livekit/root_screen_audio_backend.h`: one
  `ScreenAudioBackend` per OS opens the capture and resolves the target (everything but this process
  tree, one process tree, or a window's application), and writes interleaved float32 into a wait-free
  `ScreenAudioSink` from its own audio thread. Exactly one file per OS defines
  `RootScreenAudioBackendBuilt()` and `CreateRootScreenAudioBackend()`, chosen in `webrtc-sys/build.rs`:
  `src/screen_audio/root_screen_audio_win.cpp` (WASAPI process loopback) on Windows,
  `root_screen_audio_mac.mm` (ScreenCaptureKit) on macOS, `root_screen_audio_linux.cpp` (PipeWire) on
  Linux, `root_screen_audio_none.cpp` elsewhere. Each backend's OS is also in `SCREEN_AUDIO_OS` in
  `livekit-ffi/src/server/rootapp_audio.rs`.
- **Core** `root_screen_audio.{h,cpp}`: `RootScreenAudioCapture` attaches to a queued 48 kHz mono or
  stereo `AudioTrackSource` as its `AudioSourcePuller` (`audio_track.{h,cpp}`: the source's 10 ms task
  asks the puller for every frame and `capture_frame` refuses frames meanwhile). The pipe downmixes to
  stereo, converts other rates with `PushResampler<float>` in 10 ms chunks, and keeps a 65536-frame
  SPSC ring; the pull primes a 20 ms margin (plus what a large OS packet holds), skips a backlog,
  corrects drift on the exact ring level with the desktop's DriftResampler (6-point Lagrange, 32.32
  phase; 2 ms deadband, 0.2 % at 12 ms, up to 1 % beyond 40 ms), counts underruns and tells capture
  pauses (gaps) from late deliveries (jitter: +10 ms margin up to 200 ms, -10 ms after 60 s), fades
  3 ms at every edge (an underrun with less than 3 ms left continues that audio mirrored around its
  last frame, into the previous pull's kept tail, so the fade-out never cuts short) and splices a
  level 30 ms or more above the margin out WSOLA-style (the desktop's
  AudioSplice; the largest recent OS delivery decays by 1/64 per delivery, so a backlog in the first
  packet costs one splice, not 20 s of 0.2 % drain). Neither
  side locks, allocates or logs. A control thread per capture runs the backend's Start/Stop and the
  restarts (backoff 0.2, 1, 3, 3, 3 s, 5 per minute; none for permission or a gone target) and queues
  events; dropping the handle detaches at once and leaves the backend stop to that thread. A restart
  never widens: backends report the captured process (`ScreenAudioStartResult::process_id`), and when
  the first start captured one process or application, a restart that resolves to all audio or to
  another process ends the capture as TARGET_GONE before any of its audio reaches the source
  (`ScreenAudioTarget::restart` tells a backend it restarts the same capture). A gate in
  front of every backend drops late calls, so a backend whose Stop could not join is leaked safely;
  a stop that leaks it says so in `RootScreenAudioStopped.error` (additive field 2) and in the stats'
  last_error (INTERNAL), and so does a stop whose OS stop was not confirmed within its budget
  (`ScreenAudioBackend::StopConfirmed`, macOS: the capture no longer feeds the source but may still
  be stopping). A start that knowingly captures less than its target says why in
  `RootScreenAudioStarted.limitation` (additive field 6, `ScreenAudioStartResult::limitation`).
- **Windows backend**: `ActivateAudioInterfaceAsync(VAD\Process_Loopback)` with
  `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` (structs declared locally; Mmdevapi and avrt loaded
  at run time from System32 only, so nothing new is linked), EXCLUDE (own pid) or INCLUDE (target
  pid) tree, an agile
  completion handler, shared LOOPBACK | EVENTCALLBACK at 48 kHz float32 stereo with a 100 ms buffer
  (20 ms if refused), E_UNEXPECTED retried with re-activation; an MMCSS "Audio" thread owns every COM
  object, writes packets straight from `GetBuffer` (SILENT as silence, DATA_DISCONTINUITY counted) and
  reports device loss, stream failures and an exited target. Process loopback exists from Windows 10
  2004 (build 19041; the SDK header's 20348 is only its documented minimum): below that the start is
  UNSUPPORTED without trying. E_NOTIMPL, ERROR_NOT_SUPPORTED and ERROR_FILE_NOT_FOUND are
  UNSUPPORTED on a first start (START_FAILED on a restart: that capture already ran, so they pass),
  E_ACCESSDENIED PERMISSION_DENIED, device-lost codes DEVICE_LOST and anything else
  START_FAILED (both restartable, so an audio-service restart recovers on Windows 10 too). The open
  (activations, retries, Initialize) has one 4.8 s budget inside Start's 5 s, and the activation wait
  also wakes on the stop event, so a timed-out start joins its thread. A target counts as running
  when `PROCESS_QUERY_LIMITED_INFORMATION` opens it or is denied; include mode watches its exit with
  a SYNCHRONIZE handle, or polls its exit code every 500 ms where SYNCHRONIZE is stripped (an
  anti-cheat driver) or a wait fails; a target gone while starting is TARGET_GONE, and where neither
  works the started description says the exit is not watched. A target that is an ancestor of Root
  (a File Explorer window when explorer.exe started Root; a browser window when a root:// link
  started Root from that browser) captures nothing, at process scope, as the started description
  says, `limitation` is TARGET_IS_ROOT_ANCESTOR (the desktop tells the user) and the log warns: its
  tree holds Root's own playout, and all audio would be wider than the user's pick
  (`ScreenAudioAncestors` walks a Toolhelp snapshot; a parent created after its child is a reused
  pid).
- **macOS backend** (macOS 13+, else UNSUPPORTED): the desktop's managed `SckLoopbackCaptureService` in
  native code. An audio-only `SCStream` on the main display (capturesAudio, 48 kHz, 2 channels,
  excludesCurrentProcessAudio, 2x2 px, 1 s frame interval, queue depth 3); a window target resolves
  by CGWindowID, then title, to its owning application and a process target to the application with
  that pid (`includingApplications`, scope APPLICATION, its exit watched with `DISPATCH_PROC_EXIT`:
  TARGET_GONE); anything else, or Root's own window, `excludingApplications` this process tree
  (`proc_listpids` walk from `getpid()`). `CGPreflightScreenCaptureAccess` false or
  SCStreamErrorUserDeclined is PERMISSION_DENIED (it never prompts; the video share does). The audio
  queue reads the buffer list into a preallocated 8-buffer list and interleaves planar audio into a
  preallocated 4096-frame buffer; a wrong format is noted there and logged once off the audio queue,
  a different rate restarts at that rate (remembered for that share only: a start the host asks for
  forgets it; a rate that is not a multiple of 100 Hz is UNSUPPORTED). A stop from the menu bar is
  PERMISSION_DENIED, a lost display (-3815) DEVICE_LOST; the codes count only in
  SCStreamErrorDomain. A buffer whose presentation time is more than 2 ms from where the previous one
  ended (SCK dropped audio) counts as a discontinuity, unless it is 0.5 s or more later
  (`kScreenAudioIdleGapSeconds`: delivery paused while nothing played). A start past its 5 s budget
  is abandoned to its completion handler, which stops the stream as soon as SCK finishes starting
  it (an atomic handshake decides who was first), and nothing is stopped or removed while that start is
  outstanding; start and stop completion blocks hold the stream, output and queues until they run;
  a close whose callback outlives 2 s still sends stopCapture and keeps every object alive, and a
  stop SCK does not confirm within 2 s is reported (`StopConfirmed`, stopped.error). Manual
  reference counting; every ScreenCaptureKit reference is newer
  than the deployment target, so it is weak and the library still loads on macOS 10.15-12.
- **Linux backend**: the desktop's managed `PipeWireLoopbackCaptureService` in native code, without
  pw-dump/pw-link. libpipewire-0.3 is dlopened (nothing new in NEEDED; no library: UNSUPPORTED; no
  daemon, no socket or a refused connection: START_FAILED, restartable; EACCES: PERMISSION_DENIED);
  only the exported functions come from it, the
  registry, bind, link-factory and loop-timer calls are the headers' inline interface calls. A first
  start waits for the daemon to list its graph and fails as UNSUPPORTED, with a message that says
  so, when it has no audio sink and no playback stream: PulseAudio plays the audio and PipeWire runs
  for video only (Ubuntu 22.04, Debian 12 KDE/Xfce), so a capture would stay silent. Restarts skip
  that check (a restarted daemon lists its devices late). During a start only EPIPE and EACCES/EPERM
  core errors fail it; other core errors are logged. The headers are the
  libwebrtc sysroot's (`debian_bullseye_*-sysroot/usr/include/{pipewire-0.3,spa-0.2}`, PipeWire
  0.3.65), else the host's `libpipewire-0.3-dev`. A `pw_thread_loop` runs a `pw_stream` capture node
  (F32LE 48 kHz FL/FR, MAP_BUFFERS | RT_PROCESS, never autoconnected; node.name
  root-screen-share-audio, media.role Screen) and a registry listener that binds every node and client
  for its full properties and links the node itself (`link-factory`, `object.linger=false`, so the
  links die with the client) to every Stream/Output/Audio node not in this process tree (node or
  client `application.process.id`, a /proc walk from `getpid()`) and not a downstream re-emitter of its
  audio (effects apps, loopbacks, filter-chains: the managed `PipeWireGraph.DownstreamReemitters` taint
  walk; PipeWire's loopback, filter-chain and echo-cancel modules set node.link-group, which the
  walk follows; a virtual sink without a device.id or a link group that Root feeds and nothing
  captures taints its own client, the one way such a sink, a combine-style sink that sets no link
  group for example, can pass Root on unseen), FL to FL, FR to
  FR, MONO to both, other layouts alternately. A surround stream (an app stream on a 5.1/7.1 sink
  takes the sink's layout) links only FL and FR: links have no gain, so folding FC, LFE and the rears
  in would double an upmixed stereo stream and comb-filter its delayed rears, and the graph cannot
  tell upmixed stereo from real surround; dialogue mixed only into FC is lost on such sinks (managed
  parity). Graph changes are coalesced by a
  core sync and planned on the loop (`root_screen_audio_pipewire_graph.h`, platform-free), so new
  streams are linked as they appear and a stream that turns into a re-emitter is unlinked; a link the
  daemon refuses or removes is made again at once twice, then after 1 s and every 5 s while the plan
  wants it (failures 10 s apart start over), woken by a one-shot loop timer. The process callback
  (data loop) only dequeues, writes into the sink and queues; it also compares the driver's
  `spa_io_position` cycle (kept from io_changed) with the end of the previous one and counts a
  missed cycle (an xrun) as a discontinuity; a jump of 0.5 s or more is the node sitting out cycles
  with nothing linked (idle, not counted). A stream error, a disconnect or the daemon closing the
  connection is DEVICE_LOST (restart). Window and process targets capture everything but Root (the
  portal never names the window), said in the started event's description.
- **FFI** (`livekit-ffi/src/server/rootapp_screen_audio.rs`): `RootScreenAudioStart/Stop/Stats`
  (requests 1012-1014), `RootScreenAudioEvent` (event 1001, polled every 20 ms from 100 ms after the
  start); an owned capture handle. Caps bit 13 SCREEN_AUDIO where an OS backend is built (Windows,
  macOS and Linux: 16383 on 64 bit). `NativeAudioSource::attach_screen_audio` (libwebrtc) keeps cxx
  types out of the FFI crate.
- **Upstream fixes** the review found: `~InternalSource` resets its task queue first, so the 10 ms
  task never runs on members already destroyed (on GCD queues only tasks not yet started are
  stopped); `NativeAudioSource::capture_frame` drops its completion box when C++ refuses the frame
  (an attached puller refuses every host frame).

Tests (device-free): webrtc-sys `root_screen_audio_tests` (the pipe on a manual clock: pass-through
bit for bit, downmix, 44.1 kHz conversion, ±100-2000 ppm drift without underruns, gaps vs jitter,
target decay, backlog skip, large packets, phase-aligned trim, overflow, clipping and non-finite
samples, a 3 ms fade-out at every underrun whatever is left; the capture against a real queued
source with scripted fake backends: events, restarts and their budget, a restart that would widen a
window share or switch process (TARGET_GONE) and one that narrows a system start, the started
limitation, stop, a stop whose backend cannot stop, a stop the OS did not confirm, a shutdown that
never waits, a backend that cannot stop, refused second
captures and host frames, source checks, a 44.1 kHz mono backend; `ScreenAudioAncestors`: the
parent chain, reused pids, cycles); livekit-ffi `rootapp_screen_audio` (mapping, the stopped error,
the started limitation, routing and handle lifetime, refusals, and the real request path with the
fake tone backend, which every capture a test makes gets: `new_capture` switches it on under
`cfg(test)`, so no test can open a real OS capture); webrtc-sys `root_screen_audio_pipewire_graph` (the
Linux backend's graph rules on every OS: channel mapping, a 5.1 stream, client pids, own streams,
effects apps, loopback link groups, virtual and hardware sinks, chained effects, direct captures,
the capture node itself, PulseAudio-only graphs, /proc stat parsing and the process tree). The OS
backends themselves only run live. CI run 38356219034 compiled the macOS and Linux backends (at
1ad39ef9) for osx-arm64/x64 and linux-x64/arm64; the 2026-10-13 review and verification fixes to
them were written on Windows and only syntax-checked there (clang-cl against stub SDK and PipeWire
headers), so the next CI build is their first real compile.

## Echo model (`rootapp/echo-model`, 2026-10-13)

DTLN-aec-128 (Westhausen 2021, MIT) as AEC3's neural residual echo estimator in the platform engine's
APM (`BuiltinAudioProcessingBuilder::SetNeuralResidualEchoEstimator`, m150). The managed engine's
`AudioProcessingModule` never has it. Research, design and evaluation: the desktop session's
scratchpad `echo/design.md` and `echomodel/design.md`.

What runs:

- `rootapp-echo-model` (new crate, rustfft only): a bounds-checked TFLite reader and an
  allocation-free executor for DTLN-aec's two float graphs, the streaming model (512 window, 128 hop,
  16 kHz) and `Estimator`, the research's estimator spec (scratchpad `echo/estimator-spec.md`, same
  arithmetic in the same order as its reference `echo_model_estimator.h`), per 4 ms AEC3 block:
  - `ModelFraction`: every second block DTLN-aec runs on AEC3's band-0 linear output and render
    (channel 0) and the per-bin power gain it applied (`GainTracker`, 24 ms old) becomes
    `fraction = min(1, 1.5 * (1 - gain))`, held until the next hop (a non-finite gain resets the
    model, fraction 1);
  - `Rule`: `R2 = R2_unbounded = max(model, floor)` per bin and capture channel, with
    `model = far end active ? E2 * fraction : 0` and `floor = S2 / ERLE / today's enr_suppress`
    (ERLE tracked like AEC3's subband ERLE on capture channel 0, the render spectrum a 128-point
    sqrt-Hann DFT; the enr_suppress per bin and state comes from the config AdjustConfig receives),
    raised to Y2 in a block whose capture peaks at 32000 while y - e peaks over 20000 (saturated
    echo) and to E2 for 5 blocks after the linear output carried 1.5x the mic power with the far end
    active outside dominant nearend (diverged filter), and for the onset guard's blocks after the far
    end turns active outside dominant nearend. The x2 baseline's render gate is gone.
  `estimate` and `reset` never allocate, lock, log or panic. Dot products: AVX2 with FMA when the CPU
  has both (`is_x86_feature_detected!`), SSE2 otherwise, NEON on aarch64, a scalar fallback (which
  keeps the reference's summation order). The workspace builds this crate and rustfft at opt-level 3
  (the release profile is `z`).
- Far-end activity (`RootEchoFarEnd`, one per decorator, both variants): every render entry point
  stores the decorator clock's time of a frame whose mean square over all channels is over 1e-6
  (-60 dBFS; 1073.74 in int16 units); every capture entry point stores its time in the variant in
  use before calling it, and the estimator reads both (relaxed atomics): active while the capture
  frame is at most `max(100 ms, D + 60 ms)` after the last active render frame; onset guard
  `max(60 ms, D + 40 ms)` in 4 ms blocks, rounded up. D is the stream delay the voice engine sets
  when above 0, else AEC3's own estimate (`GetStatistics().delay_ms`, read by the controller on the
  worker thread every run while the mode is not OFF), else 0. The warm-up replay leaves the render
  time alone and gives each replayed capture frame its kept time.
- Weights: `rootapp-echo-model/weights/dtln_aec_128_{1,2}.tflite`, float32, from
  `https://github.com/breizhn/DTLN-aec` at 9d24e128b4f409db18227b8babb343016625921f
  (`pretrained_models/`), embedded with `include_bytes!` (7.27 MB; the library grows by about that):
  `_1` 1,996,600 bytes, sha256 `8d241b3a732af8ca140b2e30043e56a6c3c7800c46e22c26f4eea2f70974ad1e`;
  `_2` 5,277,012 bytes, sha256 `350bb01a1152ae3cabe09fe5e868ef2f7d8b988a9f22aae44f140195f6493126`
  (equal to the research copies). Decoded once per process (`prepare`, about 10 ms, on a thread of
  its own the first time a decision wants the model). f32, as the spec recommends (f16 would save
  3.6 MB and move 4-5% of bins). webrtc-sys depends on the crate on desktop targets only.
- `RootResidualEchoEstimator` (webrtc-sys `root_echo_model.{h,cpp}`) wraps a model session and
  passes it render, y and e of channel 0, S2, Y2 and E2 of every capture channel, the dominant
  nearend flag and the far end's state; its `AdjustConfig` first hands the session the config's own
  enr_suppress per state, then sets Google's suppressor tuning for its estimator (masks 0/1/0.3,
  max_inc 100, max_dec_lf 0, nearend_average_blocks 1, dominant-nearend 0.5/2, high-frequency
  limiting 24/3; AEC3 applies it only while it consults the estimator). Each inference block is timed
  (steady clock) for the stats and the kill switch.
- Two APMs: `RootApmDecorator` runs either the factory's APM exactly as before (`BuildRootApmVariant`
  without an estimator) or the same with the model; both share one `RootCaptureProcessor` through a
  forwarder (`RootCaptureProcessorRef`), so the voice chain, denoiser, speech detector and monitor
  never notice a switch. `StageVariant` builds and configures the other one on the worker thread
  (config, policy, formats, stream delay) and warms it there: the last 8 render and capture frames
  (kept by the audio threads in seqlocked rings) are replayed into it with its forwarder off (the
  processor never sees them), each through the interface it came in by (the APM's int16 and float
  paths do not share all their state: a float replay before the voice engine's int16 frames put a
  2000-step glitch into the switch frame, as stage 2's float-only warm-up would have), so its delay
  lines, high-pass filter and render history continue the old one's. The next
  `ProcessStream` switches at its frame boundary after replaying what was kept since (at most the last
  2 frames of each direction); `ProcessReverseStream` switches only while capture has been idle for
  100 ms, and the worker thread itself once neither direction ran for 100 ms (a call that ended with
  the model on: no frame would take the staged APM, review M2). One thread switches at a time, and none
  while the APM a switch replaced is still waiting to be released. A frame is counted as kept only
  after its forwarded call, so a switch never replays a frame the old APM is still processing. Forwarded
  calls read the APM in use lock-free under a two-counter grace period: a reader counts itself in the
  epoch it read, reads the epoch again and moves over while it changed (review M1: a reader stalled
  between the two steps across two switches held an APM the second collection released); the replaced
  APM is released on the worker thread once no call can use it. Audio threads never build, configure,
  warm or release an APM. A switch still starts AEC3 over.
- `RootEchoModelController` (worker thread; posted runs and a 250 ms poll, 20 ms while a switch is
  under way): mode from `RootAudioControl.echo_model` (OFF until set, back to OFF at every session
  start and end; since `rootapp/prod-cleanup` AUTO from every session start, OFF outside a session);
  AUTO by the playout device's kind (on for SPEAKERS, LINE_OUT, DISPLAY_AUDIO); the
  measured-echo rule (AUTO while AEC3 runs alone: AEC3's fullband ERLE from `GetStatistics` every
  250 ms, a sample counting after 20 new capture frames, 8 samples of 3 dB or more within the last 40
  latch the device on until the session ends; never off by measurement; a device without a key is
  never measured); the kill switch (inference wall time over 2 ms in more than 10 of 1000 inferences,
  two windows in a row, the first 100 of an estimator skipped: off until the session ends, and a
  session reset starts the windows over); switches wait for a pause in speech (at most 3 s). The model
  is prepared the first time a decision wants it (not for AUTO on headphones), and is off (reason
  UNAVAILABLE) while the APM's echo canceller is off. A session reset keeps the device only while a
  platform playout is open, so a call never starts on the last call's device kind.
- Device kind, at every platform playout open (AdmProxy reports opens and closes on its device thread,
  outside its mutex, with the GUID and name the selection resolved to, read with the resolution),
  read-only: Windows, the endpoint's `PKEY_AudioEndpoint_FormFactor` (the console default
  for "default") and Bluetooth when a devnode ancestor's instance id starts with `BTH` (CfgMgr32:
  `CM_Locate_DevNodeW("SWD\MMDEVAPI\<endpoint id>")`, `CM_Get_Parent`, `CM_Get_Device_IDW`; links
  `cfgmgr32`); macOS, `kAudioDevicePropertyTransportType`, then for built-in devices the output
  `kAudioDevicePropertyDataSource` (`ispk`/`espk` speakers, `hdpn` headphones, `line` line out) and
  otherwise the first output stream's `kAudioStreamPropertyTerminalType`, keyed by the device UID;
  Linux UNKNOWN (the PulseAudio ADM's context is private), keyed by the ADM's device name ("default:"
  and index 0's name for the default).
- FFI: control field 15, stats 114-124 (`RootAudioNativeStats.echo_model_*`), capability bit 14
  ECHO_MODEL wherever the factory builds its APM (desktop; `kRootEchoModelSupported`). A control that
  sets `echo_model` without the controller (the APM failed to build) is answered "not supported".

Env override: none natively. The desktop reads `ROOTAPP_ECHO_MODEL=off|on|auto` and sends it as
`RootAudioControl.echo_model` (its default is AUTO); an old desktop never sets the field, and native
then runs exactly the APM of builds without the model (up to hw.18; since `rootapp/prod-cleanup` a
session starts in AUTO, so a host that sends nothing, such as the desktop's mic test, gets AUTO).

Measured on this machine (Ryzen 9 9950X3D, Windows, release, one thread; `cargo run --release -p
rootapp-echo-model --example bench`): a block that runs the model 105 µs mean, 117 µs p99 with
AVX2+FMA (136 scalar, 136 SSE2), a block that only runs the rule 2.8 µs mean, 7 µs max (almost all
the 65 x 128 render DFT), so 135 µs of estimator per 10 ms with AVX2+FMA, 174 µs with SSE2. The APM
at 48 kHz, stereo render and mono capture per 10 ms (`root_echo_apm_cpu_per_10_ms`): 49 µs without
the model, 188-192 µs mean, 164-168 µs median, 282-291 µs p99 with it (the x2 baseline: 184-193 /
159-162 / 277-375). The capture callback (render and capture of one frame) on the frame a switch
happens, over 8 switches with echo present (`root_echo_switch_capture_cost`): worst 1.58 ms while the
warm-up ran there (stage 2), 164-172 µs since it runs on the worker thread, 186-294 µs with the spec
estimator (steady frames: median 161-166 µs, p99 368-728 µs, the first blocks of a fresh estimator
included).

Measured-echo calibration (review m12), on the AEC Challenge's real recordings (16 kHz, 24 far-end
single-talk clips with their loopback as the render, 24 near-end single-talk clips under another
clip's far end; `root_echo_measured_echo_calibration_on_real_recordings`, ignored, with
`ROOTAPP_AEC_REAL_DIR`): without an echo path AEC3's ERLE never passes 0.4 dB, and it reads 0.2 dB
for the first 3.5-4 s of every clip. 6 dB / 12 samples latched 11 far-end clips within 10 s; 3 dB / 8
samples latches 16 (4.5-8 s), and still no near-end clip shows a single evidence sample. The other 8:
on 6 AEC3's linear filter never converges (ERLE under 1.7 dB for the whole clip), although AEC3 alone
leaks on 5 of them (15-91% of far-end windows in the research's measurements), and 2 converge too late
(10.3 s; after 12 s). No ERLE threshold can see the first 6; that needs another signal (the
reviewer's nonlinear-loudspeaker case).

The estimator spec against AEC3 alone and the x2 baseline it replaced, measured in this harness
(leak share / worst 50 ms window in dB over the mic noise floor; the x2 column with a temporary
switch that ran the old estimator in the same build, not committed):

| scene (test) | AEC3 alone | x2 baseline | spec |
|---|---|---|---|
| two-tone beeps (`is_never_worse_on_tonal_render`) | 8.3% / 23.1 | 8.3% / 55.6 | 0.0% / -3.1 |
| beeps, mic noise x5 (`on_beeps_in_a_noisier_room`, worst) | 23.6 | 52.1 | 8.6 |
| call-start bells (`on_call_start_bells`) | 88.9% / 64.4 | 44.4% / 64.3 | 38.9% / 64.3 |
| loud laptop (`on_the_loud_laptop`) | 10.0% / 35.0 | 2.5% / 34.7 | 2.5% / 22.7 |
| loud laptop: double talk / near only level (dB) | 106.0 / 110.6 | 110.3 / 109.9 | 108.2 / 110.6 |
| 48 kHz call-start chimes (research scene, `on_the_48_khz_chime_scene`) | 7.7% / 47.9 | 1.3% / 16.8 | 0.0% / 5.4 |
| 16 kHz fresh chimes (`on_the_16_khz_speech_scenes`) | 43.6% / 44.3 | 8.9% / 44.3 | 7.9% / 44.3 |
| same: far end silent 1 s, near SNR (dB) | 24.5 | 21.8 | 24.5 |
| 16 kHz music | 33.2% / 26.2 | 8.4% / 29.5 | 1.0% / 15.3 |
| 16 kHz skipped cycles | 5.9% / 20.4 | 1.2% / 37.3 | 0.0% / -0.2 |
| 16 kHz double-talk laptop: near kept (dB) / STOI | 4.3% / 35.9; -2.7 / 0.806 | 0.0% / 5.4; -1.7 / 0.832 | 0.0% / 2.9; -1.8 / 0.838 |
| 16 kHz drift 100 ppm | 7.0% / 43.3 | 0.0% / 2.1 | 0.0% / 5.1 |
| 16 kHz loud and distorted: near kept (dB) | 5.2% / 13.7; -3.3 | 0.9% / 13.9; -3.1 | 0.0% / 4.9; -3.7 |
| 16 kHz near after far: far quiet SNR (dB) | 5.0% / 20.6; 23.0 | 0.0% / -0.2; 12.5 | 0.0% / -0.8; 23.0 |

Every leak and worst-window number the spec lists for its research probe (the native scenes at 48
kHz, the 16 kHz scenes)
comes out the same here, to 0.1 dB. The bells' worst window stays: it falls where AEC3's delay
estimate jumps on tonal render and AEC3 does not consult the estimator (spec 2.2). The x2 column's
idle noise dropped 2.1-3.0 dB under AEC3's on the 16 kHz scenes; the spec's stays within 1 dB.

Tests (device-free; no FFI server; LK_HW_AUDIO_TESTS unset):

- `cargo test -p rootapp-echo-model --release` (17, 2 ignored): the weights' hashes and sizes,
  DTLN-aec-128's parameter count, malformed models as errors, the model's fraction against the
  research runtime's x2 fractions on a synthesized scene (`tests/data/golden-x2.f32`: max 2e-3, mean
  1e-5), every kernel against the scalar one, dot products against f64, reset repeats bit for bit,
  hostile input, no allocation or free in 4000 blocks with resets and hostile blocks (counting
  allocator); the spec's unit tests on synthetic hook inputs with a fixed fraction (`tests/rule.rs`:
  no far end and no linear estimate is no residual, the model only while the far end is active,
  saturated echo for its block only, a diverged filter for that block and the next four and not in
  dominant nearend, the onset guard for exactly its blocks outside dominant nearend, the floor's
  scale X / 0.4 and X / 1.1 in bin 3, X / 0.1 and X / 0.3 in bin 10, per capture channel); the port
  check (`tests/golden.rs`): the research probe's recorded trace of the native beeps scene
  (`tests/data/native-beeps.emr`, 3.6 MB, synthesized, sha256 `fdc9cc2b...ce036a6d`; 1998 calls, 4
  far-end onsets, 147 in dominant nearend, 11 divergence triggers) replayed with its far-end flags:
  R2 bit-exact on every bin with the portable kernel, 96.3% bit-exact and all bins within the spec's
  1e-3 (min(Y2, E2) + 1) with AVX2+FMA. Ignored unless asked for (`-- --ignored`; each fails without
  its data): the double-talk laptop trace (`ROOTAPP_ECHO_GOLDEN_DIR`, the research's
  `echo/out/golden`; derived from the AEC Challenge's synthetic set, so not committed; sha256
  `51cd911f...c29e271f`; 1250 calls, 333 in dominant nearend, 41 divergence triggers: bit-exact with
  the portable kernel, 91.5% bit-exact and all within the tolerance with AVX2+FMA), and the executor
  on the authors' published outputs (`ROOTAPP_DTLN_REFERENCE_DIR`, a DTLN-aec checkout; DTLN-aec-512,
  the only size they published): within 1 LSB on all three clips, one reference sample that clipped
  excluded (soundfile writes x * 32768). `cargo clippy -p rootapp-echo-model --all-targets --release
  -- -D warnings`, also for `aarch64-pc-windows-msvc` (the NEON path).
- `cargo test -p webrtc-sys --lib --release root_echo` (30, 3 ignored): Windows and macOS
  classification tables with fake property values and Bluetooth instance ids, the policy (modes,
  kinds, the measured-echo window, frozen statistics, a device without a key), the kill switch's
  windows and a session reset starting them over, AdjustConfig, an unset mode and OFF bit for bit
  equal to the APM built as before (6 s scene through the whole voice chain; also through the int16
  interface), switches on and off at a frame boundary with no level step or sample jump and the voice
  chain untouched (processed frames, resets, flushes, format; float and int16), on, off and on again
  with echo present with the variant without the model bit for bit a fresh APM fed the same frames
  from its warm-up on, AUTO following the device and the session reset (the device forgotten once the
  playout closed), the worker switching off after the audio stopped and then not polling, a reader
  stalled inside the grace period across two switches (the real decorator, the review's interleaving),
  measured echo turning it on (4.3 s into a clipping laptop echo) and keeping it per device, no echo
  path never turning it on, the model off while echo cancellation is, the kill switch, inference stats
  (p99 NaN after a session reset), the far end active exactly while a capture frame is within the
  hold of the last render frame above -60 dBFS (float and int16, stream delay 0 and 80 ms: hold 100
  and 140 ms, onset guard 15 and 30 blocks), the spec's bars on the native scenes (8.1: loud laptop
  leak at most half of AEC3 alone's, worst window 5 dB under it, double talk within 3 dB, near only
  within 0.5 dB; bells leak at most 0.6x, worst at most +1 dB; beeps worst at most +1 dB and 10 dB,
  leak not above; beeps with 5x the mic noise worst 6 dB under), switching 40 times under running
  render and capture threads and a statistics reader, the capture callback's cost on switch frames,
  playout opens and closes reaching the sink, the control refused without the model, and the APM's
  CPU per 10 ms. Ignored, each failing without its data: the spec's 16 kHz real-speech scenes with
  their 8.2 bars (`ROOTAPP_ECHO_FIX16_DIR`, the research's `echo/data/fix16`: LibriVox speech from
  the AEC Challenge's synthetic set, 13.4 MB, not committed; metrics ported from the research's
  `eval.rs` in `root_echo_metrics.rs`, STOI included), the research's 48 kHz chime scene
  (`ROOTAPP_ECHO_SIM48X_DIR`, never worse than AEC3 alone), and the measured-echo calibration
  (`ROOTAPP_AEC_REAL_DIR`).
- `cargo test -p livekit-ffi --lib --release rootapp_audio` (33), `platform_audio`, `cargo test -p
  livekit --lib --release platform_audio` (29): wire values, control and stats conversions, bit 14
  (caps 24575) tied to `root_echo_model_supported()`.

Notices: the DTLN-aec MIT text is `rootapp-echo-model/weights/LICENSE`, named in `NOTICE`, and
appended to the `LICENSE.md` every `ffi-*.zip` carries (`build-rootapp-common.sh`,
`build-rootapp-windows.ps1`). The weights were trained on the DNS and AEC Challenge data (CC BY 4.0,
CC0, LibriVox and others); whether those terms reach a trained model is open for a legal look.

What CI has to prove on macOS (x64 and arm64, deployment 10.15/11.0) and Linux (x64, arm64): build and
link (`root_echo_model.cpp` with CoreAudio and CoreFoundation on macOS; only its and
`root_audio_processing.cpp`'s macOS and Linux paths were checked here, syntax-only with clang-cl and
stub CoreAudio/CoreFoundation/pthread headers, and `adm_proxy.cpp`, `audio_device_controller.cpp` and
`peer_connection_factory.cpp` were not); `cargo test -p
rootapp-echo-model --release` (the NEON kernel runs nowhere yet; the golden tolerance may need to grow
on another libm, and the beeps trace is held bit-exact only on x86-64 Windows, elsewhere to the
spec's tolerance); `cargo test -p webrtc-sys --lib --release root_echo` (the bit-for-bit test assumes
one APM build is deterministic per platform); the CPU test's numbers on an Intel Mac and an older
Linux laptop. Live (Jesse): the device kinds on real hardware (AirPods, a Bluetooth speaker, a USB
headset, HDMI audio, a combined laptop jack), a switch mid-call, and the kill switch on a slow machine.

Building this worktree on Windows: the prebuilt's deepest abseil header
(`absl/time/internal/cctz/include/cctz/civil_time_detail.h`) passes MAX_PATH under
`livekit-rust-sdks-echo\target\release\build\scratch-*\out\livekit_webrtc\...`, and cl.exe fails with
C1083. Point `LK_CUSTOM_WEBRTC` at a short junction to the extracted prebuilt (`win-x64-release` with
`include`, `lib`, `webrtc.ninja`).

## hw.10 (2026-10-13)

Publisher SDP only; caps stay 32767. The send-codec pin covers external-source audio tracks only, so
the platform engine's mic follows the server's answers again (DTX, channels and payload order as the
later answers give them; not yet checked in a live call), and every factory's environment turns
`WebRTC-SdpBundlePayloadTypeCollisionCheck` off. Both are described in Phase 2 of "Native call audio"
(from "The server's answers change codecs too").

## Auto sensitivity (`rootapp/auto-sensitivity`, hw.11)

The voice chain's auto sensitivity (`rootapp-voice-dsp/src/auto_sensitivity.rs`, bit for bit the
desktop's `AutoSensitivity.cs` through the golden vectors) learns the noise floor of the denoised voice
before the voice gain; the gate opens 8 dB over it, never below a minimum that rises with at most 11 dB
of the gain. hw.11 changes three things.

- The minimum is -54 dBFS (`AUTO_SENSITIVITY_MINIMUM_DB`, was -60; the desktop's
  `AutoSensitivity.MinimumDb`). A headset with the Effects SDK on (Windows, 10/14) kept its denoised
  floor under minimum - 8 dB for whole calls, so the threshold sat at the minimum and desk taps opened
  the gate.
- A hardware mute teaches nothing. A muted device delivers digital silence; counted as a level it
  dragged the floor to -90 dB within seconds, and after the unmute the floor climbed back at 1 dB/s
  (39 s to -50 dBFS of room; 41 s in the chain with its gate). `RootApmDecorator::ProcessStream` (int16
  and float) checks the raw capture before the APM: every sample of every channel at or below
  `kRootCaptureSilencePeak`, 1e-5 of full scale (-100 dBFS). That is under one int16 step (-90.3
  dBFS), so the int16 frames the voice engine passes must be all zeros, while a float capture still
  counts with a muted converter's dither or a resampler's decaying residue; a live mic chain never gets
  that low (even -100 dBFS RMS of self-noise peaks about 10 dB higher within 10 ms). Neither the APM
  output nor the denoised level can decide: AEC3 can add comfort noise (its noise floor is -96 dBFS)
  where it suppresses echo, and the Effects SDK can return near-silence on a live mic. The decorator
  stores the flag in `RootCaptureState::capture_silent` before the inner APM runs, and the capture
  processor takes it (exchange) into `DspSettings::capture_silent`, so a frame that did not come
  through the decorator never sees a stale one. Silent frames leave the floor and the window of recent
  levels alone and get the unchanged floor's thresholds (with the gain); after `relearn_silence_ms`
  (500 ms) or more of them the next live frame empties the window and restarts the 10 dB/s startup rise
  for 2 s, as at the start of a call. 30 s of mute then -50 dBFS of room: back at the room + 8 dB in
  0.9 s from a -60 dB floor or a fresh one (0.94 s in the chain), 1.9 s from -70 dB (from -75 dB the
  startup rise covers 20 of the 25 dB: 5.9 s). Mute in the app is the transmit gate after the chain:
  the mic still delivers the room, so auto sensitivity runs exactly as unmuted.
- `RootAudioControl.auto_sensitivity_minimum_db` (16) sets the minimum, so later tuning is a desktop
  change. The controller stores it as sent; the chain clamps it every frame to -80..-30
  (`sanitize::auto_sensitivity_minimum_db`, non-finite is the default) and a session start forgets it.
  `RootAudioStats.auto_sensitivity_minimum_db` (125, with the DSP stats) is the value applied, through
  the same function (bridged as `root_auto_sensitivity_minimum_db`). No capability bit (caps stay
  32767): a library that predates the field skips it as an unknown field and keeps -60, and the stat's
  absence tells the desktop so.

Golden vectors regenerated from desktop e7e38e124: 26 with `capture_silence_8k` (a 300 ms and a 500 ms
mute, zeros of a live capture, the flag over -96 dBFS residue; 8 kHz keeps the set at 12,543,311
bytes, under 12 MiB). Bit 1 of the settings byte that holds mic_muted carries capture_silent. All 26
match bit for bit (0 ulp in every value).

Tests: `auto_sensitivity` (the minimum, 30 s of mute then room noise and the 39 s it took counted as a
level, the thresholds through a mute at any gain, the window, 49 vs 50 frames, the relearn's window, a
near-silent level of a live capture), `chain` (the mute and the unmute with the gate, a software mute
leaves auto sensitivity exactly as unmuted, the minimum and its range), capture_silent and the minimum
in the no-alloc and no-panic runs; `root_capture_processor_learns_nothing_from_a_frame_flagged_silent_*`,
`root_capture_processor_applies_the_auto_sensitivity_minimum_it_is_given`,
`root_apm_decorator_flags_digital_silence_on_the_raw_capture` (the real APM, int16, mono and stereo,
one int16 step is live), `root_apm_decorator_float_capture_is_silent_up_to_minus_100_dbfs`,
`adm_proxy_controller_stores_the_auto_sensitivity_minimum_*`, and the livekit and livekit-ffi
conversions.

## hw.11 (2026-10-14)

Auto sensitivity only; caps stay 32767. The -54 dBFS minimum, hardware mute detection on the raw
capture with a relearn after it, and the minimum as control field 16 with stat 125: see "Auto
sensitivity" (bc267532 the voice chain and its golden vectors, b2980e32 the decorator, controller,
livekit and livekit-ffi).

## Voice gain (`rootapp/voice-gain`, hw.12)

The voice chain's automatic gain (`rootapp-voice-dsp/src/voice_gain.rs`, bit for bit the desktop's
`VoiceGain.cs` through the golden vectors) only boosts, towards a target for the talker's level, from
the gain remembered for the mic. Senders got quieter for long stretches (a 10/13 call: 16 -> 1 dB within
30 s of talking): the level was a 1.5 s power average of the speech frames, loud moments pulled the gain
down at 6 dB/s, it came back only while talking, and one sound in a pause could hold the room cap down
for 10 s or more. hw.12 follows WebRTC AGC2's adaptive digital controller:

- The talker's level is the average frame level in dB of their speech over about 4 s of it (AGC2's
  leaky weighted average: per speech frame sum = sum x leak + level x w, weight = weight x leak + w,
  leak 1 - 10/4000), each frame weighted by its speech probability (1 without one). 4 s is AGC2's
  value and a sentence or two of speech; a 1.5 s laugh 14 dB over the speech moves it about 2.5 dB
  where the power average moved 8. Target -25 dBFS of that level: on real speech the dB average sits
  about 3 dB under the power average (median 3.2 dB on 40 clean near-end clips, 3.1 on 24 real ones),
  so steady talkers come out where -22 put them.
- `VoiceGain::process_with_probability`: with the speech detector's probability only frames at
  `min_speech_probability` (0.5) or above are speech; without one the level decides as before. The
  chain passes `DspSettings::speech_probability` whenever it is valid, in every speech detection mode
  (the processor computes one while detection is on, which the desktop always asks for). 0.5 is the
  speech gate's confirm bound: on the hand-labelled 10/9 recordings 85-90% of speech frames over -60
  dBFS reach it and none of 4,248 click, breath and noise frames; AGC2's 0.95 keeps 69-77% of the
  speech and rejects nothing more.
- At most 15 dB (was 18; 12 left Jesse's 10/9 headset, rec2, at -29.6 dBFS, 4.4 dB under the old
  gain's -25.2, and 15 gives -26.6). 6 dB/s until 3 s of speech (was 10, which overshot more: 1.2/4.1 dB
  median/max on the clean clips against 0.7/2.9), then up 3 dB/s on speech and down 2 dB/s (was 6);
  silence holds the gain. The limiter (ceiling 0.9) clamps at once and releases at 100 dB/s (1 dB a
  frame) instead of jumping back the next frame.
- The room cap (room before the denoiser plus the gain under -50 dBFS) moves the gain no faster than
  the phase's rate (`room_limit_fall_db_per_second` is gone), and a committed room frame counts at most
  `room_burst_db` (6 dB) over the room's average (never under -90 dB): a 300 ms door slam in a pause
  moves it under 2 dB instead of 26, while a room that stays louder is followed at up to 6.5 dB/s of
  pauses. With the Effects SDK a sound the denoiser removes used to count in full: on the 10/9
  recording rec1 the room estimate jumped 23 dB on such a sound and held the gain at 0 dB for 30 s.
- Controls `RootAudioControl` 17-22 (`voice_gain_max_db`, `voice_gain_target_db`,
  `voice_gain_rise_db_per_second`, `voice_gain_fall_db_per_second`,
  `voice_gain_level_time_constant_ms`, `voice_gain_min_speech_probability`; optional floats) set the
  tunables, so later tuning is a desktop change. The controller stores each as sent; the capture
  processor hands all six to the chain every frame (`RootDspSettings.voice_gain`), which clamps them
  (`VoiceGainTuning::sanitized`: 0..18 dB, -40..-10 dBFS, 0.5..20 dB/s twice, 500..20000 ms, 0..1; a
  non-finite value is the default) before that frame's gain reset, so a remembered 18 dB starts at the
  15 dB maximum. A session start forgets them. `RootAudioStats` 126-131 are the values applied, through the
  same function (`root_voice_gain_tuning`). No capability bit (caps stay 32767): a library that
  predates them skips them and keeps hw.11's voice gain, and the stats' absence tells the desktop so.
  `MAX_VOICE_GAIN_DB` (18) stays the gain reset's bound and the highest maximum a host can set.

Before/after (hw.11's voice gain replayed against hw.12's on synthetic scenes and recordings; gain range
in the talker's speech, the dip after a burst as depth and time more than 1 dB under, and the spread of
the output speech level over 1 s windows):

| Scene | Gain range | Dip after the burst | Spread |
|---|---|---|---|
| Steady talker, 1.5 s laugh at +14 dB | 7.8 -> 3.2 dB | 8.0 dB, 7.1 s -> 2.7 dB, 10.8 s (2.2 dB with probabilities; 1.0 dB, 0.06 s when the detector rejects it) | 1.66 -> 1.36 dB |
| Steady talker, 3 s shout at +12 dB | 11.1 -> 4.1 dB | 11.0 dB, 19.7 s -> 4.4 dB, 15 s | 4.50 -> 1.36 dB |
| Door slam in a pause (Effects SDK) | 14.1 -> 1.1 dB | 14.0 dB, 19.6 s -> 0.9 dB, never 1 dB under | 6.84 -> 1.34 dB |
| Fan rising -80 -> -50 dBFS (Effects SDK) | 12.3 -> 8.9 dB | - | 4.59 -> 3.44 dB |
| 10/9 Effects SDK recording rec1 | 0 dB all along -> 12.1-13.3 dB | - | 1.56 -> 1.36 dB |
| 40 clean near-end clips (median) | 2.5 -> 2.5 dB | - | 2.10 -> 2.25 dB |
| 24 real near-end clips (median) | 0 -> 0 dB | - | 1.96 -> 1.94 dB |

Quiet talkers reach the target down to about -40 dBFS (15 dB); a -40 dBFS talker comes out at -24.7
instead of -21.9 dBFS, a -46 dBFS one at -30.7 instead of -27.7, rec2 at -26.6 instead of -25.2.

Golden vectors regenerated from desktop 54db59653 (`VoiceDspGoldenVectors/3`): 30, 12,367,145 bytes
(12 MiB limit). Bit 2 of the first settings byte marks a frame carrying a speech probability (an f32
after gain_reset_db). New `voice_gain_burst_8k` and `_p_8k` (a laugh mid-sentence, a shout the
limiter holds) and `voice_gain_room_8k` and `_p_8k` (a rising room, a door slam); `gain_reset_48k` and
`quiet_talker_48k` became `gain_reset_16k` and `quiet_talker_16k` to make room. All 30 match bit for
bit (0 ulp in every value). The speech detection check compares chains: Ring with each vector's own
probabilities against the reference, Ring against Off with any probability, Gate against Off with
constant speech.

Tests: `voice_gain` (the level in dB, a laugh mid-sentence, rejected and weighted probabilities, a long
silence, the start rate without overshoot, limiter attack and release, a door slam in a pause, a rising
room at the fall rate, the tunables' clamps and defaults), `chain` (the probability in every mode, the
host's minimum probability and target, a gain reset clamped to the host's maximum in the same frame),
tunables and probabilities in the no-alloc and no-panic runs;
`root_capture_processor_applies_the_voice_gain_tuning_it_is_given`,
`adm_proxy_controller_stores_the_voice_gain_tunables_*`, and the livekit and livekit-ffi conversions.

## hw.12 (2026-10-14)

Voice gain only; caps stay 32767. A speech-only 4 s level in dB, 2 dB/s fall, 15 dB maximum, a
burst-proof room cap and a smoother limiter, with the tunables as control fields 17-22 and stats
126-131: see "Voice gain" (50d2872c the voice chain and its golden vectors, 95379f43 the controller,
processor, livekit and livekit-ffi, 9b2409a6 the 15 dB default maximum).

## macOS playout format changes (`rootapp/mac-playout`, 2026-10-15)

Mac reports: on the first platform call after launch remote voices play far too deep ("monster"), a
rejoin fixes it. libwebrtc's `AudioDeviceMac` (webrtc-sdk m150_release 89d790b, the pinned prebuilt)
reads the output stream format at `InitPlayout` and converts every render callback to it
(`SetDesiredPlayoutFormat`, `implDeviceIOProc`: frames per callback = bytes / the opened
`mBytesPerFrame`). A later change is taken only through `HandleStreamFormatChange` on a
`kAudioDevicePropertyStreamFormat` listener, which returns before touching the output side when the new
channel count differs from `_recChannels` ("Changing channels not supported", a check meant for the
input), is not thread-safe and leaks the old converter. A Bluetooth headset that enters its hands-free
profile under a running playout (2 ch 48 kHz to 1 ch 16 or 24 kHz) while `_recChannels` is 2 (engine
init's `SetStereoRecording(StereoRecordingIsAvailable())` on a 2-channel system default input), or
whose change never reaches the listener, keeps its 2 ch 48 kHz converter: the device takes 4x (24 kHz)
or 6x (16 kHz) fewer of the call's frames than it plays, two octaves and more down, with NetEq
discarding the backlog; a return to stereo with `_recChannels` 1 plays 4x fast. The lease's playout
hold opens playout before the voice engine initializes, the room connects and the mic opens, so on a
headset the first open is in its stereo profile and the switch happens under the running playout.

`AdmProxy` now reads the open devices' formats itself (macOS only: `CoreAudioDeviceFormat`, what the ADM
reads; no reader and no change elsewhere). Before and after every successful `InitPlayout` and
`InitRecording` it reads the device the selection resolved to (the OS default's current device for a
default selection) and keeps that device's format; the device thread reads it again every 100 ms while a
direction is open. Once a playout device's format moved, whatever it settles on (even the opened format
again, since the ADM may have taken a format in between) that held for two reads reopens playout (the
restart generation, as `RestartDevices`), at most once a second, so the ADM reads the format again.
Recording changes are counted and logged only (a recording format change with the same channel count
is taken by the ADM; reopening the mic would restart a Bluetooth switch). Logs: `AdmProxy: playout
device <id> opened at <rate> Hz <ch> ch ...`, `changed format while open, A -> B`, `reopening playout
on device <id> at B`.

`RootAudioStats` 132-138 (optional, present where native reads formats, i.e. macOS):
`playout_device_sample_rate`, `playout_device_channels`, `recording_device_sample_rate`,
`recording_device_channels` (each open direction's device as the ADM opened it, 0 while closed),
`playout_device_format_changes`, `playout_device_format_reopens`, `recording_device_format_changes`
(cumulative). livekit: `RootAudioStats::device_formats: Option<DeviceFormatStats>`. No capability bit
(caps stay 32767); a library that predates the fields reports none of them.

Tests (Windows, a fake reader through the harness: `set_device_format`, `poll_device_formats`; the
harnesses run without the CoreAudio reader): `adm_proxy_reopens_playout_when_the_device_format_changes_under_it`,
`adm_proxy_reopens_playout_after_a_format_bounce_at_most_once_a_second`,
`adm_proxy_counts_recording_format_changes_and_skips_unreadable_devices`,
`adm_proxy_without_a_format_reader_reports_no_device_formats`, and the livekit and livekit-ffi
conversions. The Mac branch of `adm_proxy.cpp` passed a clang-cl syntax-only check with the macOS
define against stubbed CoreAudio declarations; not built with Apple's SDK or run on a Mac. To verify
on a Mac: AirPods (or another headset with a mic) as output and a 2-channel default input, join a
platform call; the log shows the change and the reopen, stats 132-133 go to 1 ch 16/24 kHz, voices
play at the right pitch, and the render callbacks (`renderCallbacks` on the call diag line) stay at
about 100/s.

## Voice gain: AGC2 (`rootapp/agc2`, hw.14)

Talkers still got quieter over a call with hw.12 (the gain-drift report, 2026-10-15: 30-minute
conversations built from real recordings through the real Effects SDK at 0.60):
- The room cap measured the room before the Effects SDK but chose its frames after it, so speech the
  SDK erased (a turn's start after a pause, a quiet word, the first turn after a hardware mute) was
  learned as a loud room, and the gain fell towards 0 dB and stayed there through the pauses (Jesse
  15 -> 3.2 dB for about 90 s).
- On noisier mics the cap held the gain near 12 dB for good, so a talker who faded over a long call
  faded at the output too.
- libwebrtc's AGC2 with Chrome's settings, after the denoiser, had neither problem on the same data.

The platform engine's voice gain is now AGC2 when the host asks (`gain_engine`, the desktop does by
default); the chain's own voice gain stays, fixed, for the managed engine and as the fallback.

Pipeline: APM -> pre_denoise -> speech detector -> Effects SDK -> aligner (`post_denoise_align`) ->
AGC2 -> `post_gain` (auto sensitivity, startup hold, noise gate, input volume, transmit gate).
- `RootAgc2` (`root_agc2.{h,cpp}`) is a second `AudioProcessing` with only `gain_controller2`:
  adaptive digital on, input volume controller off, high-pass filter, AEC, NS and AGC1 off. Settings
  `rootapp_voice_dsp::Agc2Tuning`: headroom 5 dB, max gain 30 dB (Chrome: 50), 6 dB/s, max output
  noise -50 dBFS; the initial gain is the last gain reset (the desktop's remembered gain for the mic).
- It lives in `RootCaptureState::agc2` (set before the APM is built; the processor makes one when
  nobody did), so the APM variants the echo model switches between share it with the voice chain.
- Built off the audio thread: `RootCaptureState::ServiceAgc2` (the controller after every
  `set_root_audio_control`, after `reset_root_audio`, and on the 10 ms speaking poll) builds an
  instance for the processor's rate when one is needed: `Initialize` at that rate and one frame of
  silence, so nothing the APM sets up on its first frames runs on the audio thread. The audio thread
  takes a staged instance at its next frame and leaves the one it replaced for the control side to
  free; nothing is taken while a replaced one waits. A gain reset is a new instance from that gain
  (clamped to the max), new settings or a new processor rate one from the gain applied last, and the
  engine or auto gain turned back on one from the last reset (as the chain's own gain). A frame at a
  rate no instance is built for yet (at most one poll after a rate change, inside the stream's 200 ms
  startup hold in practice) gets the chain's voice gain.
- The gain `post_gain` takes is measured: 10 log10(sum out^2 / sum in^2) of the frame, held while the
  input is under -70 dBFS (the instance's initial gain before any). Auto sensitivity and the gate
  learn before the gain exactly as before and add that gain. `voice_gain_db` is AGC2's gain;
  `voice_gain_settled` is the chain's voice gain having heard 3 s of speech (it keeps running on a
  copy of the frame before AGC2, measuring the room, the talker's level and the floor).
- Rates: the processor runs at the APM's rate (16, 32 or 48 kHz for 8 to 48 kHz devices; 44.1 kHz
  devices run at 48 kHz). AGC2 also runs on 8, 11.025, 22.05, 24 and 44.1 kHz frames (the APM
  resamples internally), as the processor harness can feed them.

`rootapp-voice-dsp`: `post_denoise` is now `post_denoise_align` (returns the aligned frame before any
gain in `frame` and its RMS) then `post_gain(frame, pre_gain_rms, gain_db)`; `DspSettings::gain_engine`
(`Legacy`, `External`) and `DspSettings::agc2` (the AGC2 that ran, for the limit). With `External`
`gain_db` is the host's gain (clamped 0..50, non-finite 0). The chain's voice gain (both engines):
- A frame quiet after the denoiser is room only when it was within `room_input_margin_db` (3 dB) of
  the input's own floor before it (tracked like the denoised floor: 300 ms fall,
  `room_input_floor_rise_db_per_second` 6 dB/s rise). Voices well under the talker's (loud after the
  denoiser too) still count as before. A room that jumps 21 dB at once is learned about 3.5 s later.
  Chosen on the gain-drift harness over a speech-detector bound (p < 0.3: JD 12.7 % low talk seconds
  against 12.4 %) and a 2 dB/s input floor (11.7 %, but a room rising 5.5 dB/s is missed).
- At most 24 dB (was 15); `MAX_VOICE_GAIN_DB` (the reset's and the host maximum's bound) is 24.
- `VoiceGain::limit` (`GainLimit`: none, target, max, noise, room) says what held the gain; with
  `External` the chain infers AGC2's from its gain: within 1 dB of its max, within 1 dB of its noise cap
  over the floor the chain measures, else the target.

Golden vectors `VoiceDspGoldenVectors/4` (desktop 34d9ccba7): 31, 12,573,491 bytes (12 MiB limit), all
bit for bit (0 ulp in every value); new `voice_gain_erased_8k` (the fake SDK erases a turn's first
400 ms and last 250 ms). `golden_vectors_align_and_post_gain_equal_post_denoise` runs every vector both
ways and compares frames and every output bit for bit.

Controls (`RootAudioControl`): `gain_engine` 23 (`RootAudioGainEngine` LEGACY, AGC2; legacy until set
and after every session start; since `rootapp/prod-cleanup` AGC2 from every session start and legacy
outside a session), `agc2_headroom_db` 24 (0..20), `agc2_max_gain_db` 25 (1..50),
`agc2_max_gain_change_db_per_second` 26 (1..20), `agc2_max_output_noise_dbfs` 27 (-80..-30), each
clamped where it applies, a non-finite value the default. `gain_reset_db` is clamped to 0..50 (was 18)
and then by the engine. Stats (`RootAudioStats`, after the Mac format fields 132-138): `gain_engine`
139 (the engine that ran the last frame), AGC2's settings applied 140-143, `agc2_builds` 144, and the
chain's view for both engines: `voice_gain_room_db` 145, `voice_gain_speech_level_db` 146,
`voice_gain_floor_db` 147, `voice_gain_limit` 148 (what would have shown hw.12's failure: the room
estimate jumping to speech levels and the limit at ROOM). No capability bit (caps stay 32767): a
library that predates the fields ignores them and keeps its own voice gain, and the stats' absence
tells the desktop so.

Numbers (the gain-drift harness; talk-second output p10 / p50 dBFS and the share of talk seconds 6 dB
or more under the median; dips: runs of 5 s or more at least 6 dB under the median gain). AGC2 ran in
the capture processor (`root_capture_processor_agc2_replays_a_recording`, ignored: the SDK's output of
each capture through the processor with the gate open, from 15 dB), the chain's gain through the crate
on the capture and the SDK output:

| Input | hw.12 | chain's gain (room fix, 24 dB) | AGC2 in the processor (30 dB) | AGC2 offline (Chrome, 50 dB) |
|---|---|---|---|---|
| J | -42.2 / -30.7, 20.0 % | -36.8 / -27.6, 18.0 %, dips 2 (34 s) | -32.9 / -25.7, 12.4 %, dips 1 (5 s) | -32.9 / -25.7, 12.0 % |
| J8 | -35.7 / -27.4, 13.9 % | -32.4 / -26.8, 9.0 %, dips 1 (19 s) | -30.9 / -25.7, 8.5 %, dips 1 (6 s) | -31.0 / -25.6, 8.7 % |
| JD | -39.6 / -28.9, 17.2 % | -34.9 / -27.6, 12.4 %, dips 1 (19 s) | -32.3 / -25.6, 10.7 %, no dips | -32.4 / -25.6, 10.7 % |
| L | -28.3 / -24.3, 5.7 % | -28.0 / -24.2, 5.7 %, dips 1 (20 s) | -25.7 / -21.3, 5.2 %, no dips | -25.6 / -21.3, 5.4 % |

JD (the talker falls 6 dB over the call), first and last 5 minutes: chain -24.8 -> -32.6, AGC2 -23.0
-> -27.5 dBFS. The 30 dB maximum changes nothing on these recordings. A clean voice at -45 dBFS comes
out of the processor at -22.4 to -22.8 dBFS (its frames' average in dB) at 48, 44.1 and 16 kHz.

Tests: the crate (`chain`: a host's gain is what the frame and auto sensitivity get, the chain's gain
measures under it, `post_denoise` is align then the chain's gain; `voice_gain`: erased speech is no
room, the limit, the room step; the split and both engines in the no-alloc and no-panic runs);
`agc2_tests` (a -45 dBFS voice reaches -25 +-3 dBFS at 48, 44.1 and 16 kHz; room noise at 20-30 dB of
AGC2 gain never opens the gate; every gain reset is a new AGC2 from it, clamped to its max, settings a
new one from the gain applied last, auto gain back on from the last reset; every rate and a rate
change after the poll); `root_echo_switches_keep_agc2_and_its_gain` (two echo-model switches: same
instance, the gain goes on); `root_engine_runs_agc2_from_the_controls` (the factory's wiring);
`adm_proxy_controller_stores_the_gain_engine_and_agc2_settings_until_a_reset`; the livekit and
livekit-ffi conversions.

Risks: AGC2 has no foreground gate (a TV or other voices can raise its gain; the 30 dB max and the
gate limit it); pauses and residual echo come out 2-5 dB louder; auto sensitivity's -54 dBFS minimum
gives way above 11 dB of gain, so at 25-30 dB a sound the SDK leaves at -70 dBFS before the gain opens
the gate (stationary room noise never does: the floor plus 8 dB holds); the first seconds of a call
ramp from the remembered gain; the platform and managed engines now differ.

## hw.14 (2026-10-15)

AGC2 as the platform engine's voice gain, and the chain's voice gain without the erased-speech room
(controls 23-27, stats 139-148, caps 32767): see "Voice gain: AGC2". Carries the macOS playout fix
released alone as hw.13.

## Speech gate look-ahead, hardware mute and tuning stats (`rootapp/hw18`, hw.18)

Background: the auto sensitivity bench of 2026-10-19 (15 synthetic microphone profiles of one 20-minute
conversation through the real Effects SDK at 0.60, AGC2 and the chain; the desktop's
`docs/native-platform-audio.md` has the numbers). hw.17's strict gate sends only confirmed speech, and
the detector confirms p50 60 / p90 110 ms after a word's first audible frame, so 63% of sentence starts
lost 20 ms or more (p90 69 ms) on the 12 profiles the SDK does not dominate.

### The look-ahead

- **What runs on the current frame:** the gate's level measurement (`NoiseGate::process_split`, block
  by block), the speech gate's verdict and control, auto sensitivity's update and the envelope. The
  gain that decides is applied to the post-gain frame `speech_lookahead_ms` older, kept in a delay line
  of whole frames (`frame_delay::FrameDelay`, 6 frames allocated in `DspChain::new`). The input volume
  ramp, the clamp and the transmit gate then run on that older frame, which is what is sent.
- **When:** Gate mode with a probability and no push-to-talk (`gate_audio`); 40 ms by default.
  `RootAudioControl.speech_lookahead_ms` (28) sets it: clamped to 0..60, rounded to whole 10 ms frames
  (halves up), non-finite is 40, a session start forgets it. 0 is hw.17's gate bit for bit. Off, Ring,
  push-to-talk, frames without a probability and unsupported frames never delay.
- **Edges:** starting it (or changing its length) sends that many frames of silence first; nothing is
  repeated. Ending it (Off/Ring, push-to-talk, no probability, an unsupported frame) drops what it held:
  the next frame is sent at once and up to 40 ms of audio is lost. A flush, a reset and a rate change
  drop it and start it again behind the startup hold. A frame never comes out of the delay line after
  a newer one (`the_look_ahead_starts_with_silence_ends_by_dropping_and_never_sends_old_audio`).
- **Startup hold:** it zeroes what goes in (measured and delayed alike). `held` and
  `startup_hold_frames` describe the frame sent: a stream starts with 20 + 4 held frames in Gate mode.
- **Mute:** each delayed frame carries whether the transmit gate was open when it went in; the gate is
  open for the frame sent only if it was then and is now. Muting cuts at once; after an unmute the
  first frame captured unmuted goes out 40 ms later (and rings then), so nothing captured while muted
  is sent. Push-to-talk is unchanged (no look-ahead under it).
- **Speaking (hw.15):** the frame sent carries audio, or the gate is speaking, while the transmit gate
  is open for the frame sent and it is not held. Every frame that sends audio rings (but the fade of a
  mute).
- **Gate input stats:** `gate_in_sumsq` and the block peak are the delayed frame the gain applies to
  (comparable with what is sent); `gate_in_rms`, `threshold`, `envelope` and the flags are the
  decisions on the current frame.
- **Latency:** +40 ms of capture latency in Gate mode. `RootAudioStats.speech_lookahead_ms` (154) is
  what the last frame got.
- **Invariants:** nothing a detector never confirms goes out (only the frames up to 40 ms before a
  confirmation, which is the point); at every frame Gate's threshold is Off's and its envelope never
  exceeds Off's, and each sample sent is at most what Off's gain would make of the same delayed frame
  (`gate_never_sends_more_than_off`, against Gate mode with every frame called speech on the same
  look-ahead); every frame that sends audio rings.

Measured with the bench against this chain (`autosens-bench lookahead`): `DspChain` at 40 ms and the
bench's rebuild of design C40 differ in 0 of 119,970 frames on all 15 profiles (sent energy, speaking,
speech), as do 30 ms and C30 and 0 ms and hw.17. Means over the 12 profiles: starts clipped 20 ms or
more by the gate 63% -> 19%, gate p90 clip 69 -> 38 ms, gate loss of the speech the SDK kept 3.13% ->
1.93%, speech lost 11.4% -> 10.3%; noise sent while the talker is silent unchanged (TV 317 -> 319 s of
491 s), false ring 14.3 -> 11.1 s.

The bench lines the SDK's output up 4 frames behind its input (its own stream). In the chain the SDK's
output lags its input by 30 ms at 48 kHz, 55 ms at 32 kHz and 43 ms at 16 kHz (the real SDK 1.7.2: it
returns a frame from the first call, holds 1920/1440 samples at 48/16 kHz, and bursts 480-754 samples
early on, which the aligner's pre-roll drains). At 48 kHz the detector therefore leads the gated audio
by 10 ms less than the bench assumed, and 40 ms of look-ahead in a call is about the bench's C30 (24%
of starts, p90 48 ms); 50 ms would be its C40. Not changed: 40 ms is the approved default, and the
control makes 50 a desktop change.

### The Effects SDK is not fed digital silence

After a 40 s hardware mute in a noisy room the SDK erased 62-82% of the speech in the next 30 s (fan
at -55 dBFS, mains hum, a TV); not feeding it the silent frames brought that to 0-3%. A fresh pipeline
at the unmute was worse on three profiles, so nothing restarts.

- `RootCaptureProcessor` does not call the slot for a frame whose raw capture the decorator flagged
  (`capture_silent`, hw.11) while the denoiser is wanted, and hands the chain
  `DenoiserOutput::Skipped` (kind 3): the frame is silence before the gain; the aligner neither pushes
  nor pops, the empty-output count, the slot's slow/stall counters and the startup hold's wait for the
  denoiser are untouched, and no NS gap is counted. A flush owed on a skipped frame waits for the next
  fed one. Frames skipped are counted natively (`denoiser_skipped_frames`, harness only).
- **Latency:** the SDK's stream carries on where it stopped, so its latency after the silence is what
  it was (the detector keeps its lead). What it held from before the silence (its 30-55 ms) comes out
  first, delayed by the silence: within a silence of 1-3 frames (a glitch) that is the right audio a
  little late; after a hardware mute it is the last 30-55 ms before the mute, which goes out only if
  the gate opens within those frames of the unmute (speech confirmed that fast, look-ahead included).
  Dropping it would need the SDK's latency, which it does not report (fed minus returned samples is 0
  or negative: it pads internally), and would lose real audio after every glitch.

### Tuning stats (149-154)

- `speech_denoiser_erased_frames` (149): frames of steady confirmed speech (confirmed, and 10 frames
  or more into a run the detector rates 0.3 or more; transmit open, not held) whose aligned denoiser
  output is 15 dB or more under every pre-denoise frame of the last 10 frames, which hold the frame it
  came from at any rate (the SDK's latency is at most 55 ms). A lower bound: it never counts what the
  SDK kept, nor noise the SDK removed in the 300 ms hang after speech.
- `speech_gate_cut_frames` (150): frames sent whose audio was steady confirmed speech above -66 dBFS
  (not erased) when it went in, while the noise gate was shut for the whole frame (under the
  threshold), transmit open, push-to-talk not in use.
- `auto_sensitivity_floor_db` (151, NaN while off), `open_threshold_db` (152, before the gain: auto
  sensitivity's open threshold, or the manual threshold less the gain, at least -140) and
  `threshold_term` (153, `RootAudioThresholdTerm`: FLOOR the floor plus 8 dB, MINIMUM the minimum
  lowered by the gain above 11 dB, MANUAL; `AutoSensitivity::floor_sets_open_at`).
- `speech_lookahead_ms` (154).

All with the DSP stats; no capability bit, caps stay 32767: a library that predates them skips control
28 and sends none of 149-154.

Tests: `rootapp-voice-dsp` (`speech_gate.rs`: the setting in whole frames, only in Gate mode, a word's
start sent when the confirmation comes within the look-ahead and frames before it never, the startup
hold, every edge (mode, push-to-talk, length changes, a flush, rates, an unsupported frame, a mute, no
probability) with fingerprinted frames that name their source, the property test against Gate with
constant speech on the same look-ahead, the mute and hang tests on the new timing; `frame_delay.rs`;
`noise_gate.rs` the split; `chain.rs` skipped frames keep the aligner and stall count, are no denoiser
for the hold, the two counters and the threshold stats; the golden vectors: Gate with constant speech
and no look-ahead is Off bit for bit, and with 40 ms makes Off's decisions on every frame and sends
Off's frame f - 4 at frame f wherever Off's gain held over both (13,413 frames compared);
`no_alloc`/`no_panic` vary the look-ahead and the skip), webrtc-sys
(`root_capture_processor_never_feeds_the_denoiser_digital_silence`,
`root_capture_processor_applies_the_speech_lookahead_it_is_given`, the speech tests on the new timing,
`root_voice_a_hardware_mute_never_reaches_the_denoiser_and_nothing_falls_back`,
`adm_proxy_controller_stores_the_speech_lookahead_until_a_reset`), and the livekit and livekit-ffi
conversions.

## hw.18 (2026-10-19)

The speech gate's 40 ms look-ahead (control 28), the Effects SDK no longer fed digital silence, and
the tuning stats 149-154 (caps 32767): see "Speech gate look-ahead, hardware mute and tuning stats".

## Production cleanup (`rootapp/prod-cleanup`, after hw.18)

Before the native call engine ships as an opt-in. Caps stay 32767; no FFI request, proto message or
field, or C export the desktop uses changed (proto enum values are kept for wire compatibility).

Test-only code out of release builds:

- **MF fault injection** (`LK_MF_FAULT_*`, `LK_MF_MAX_SESSIONS`): compiled in only with webrtc-sys's
  `mf-fault-injection` feature (define `LK_MF_FAULT_INJECTION`); see "Runtime switches". The support
  switches `LK_DISABLE_MF_ENCODE`, `LK_DISABLE_MF_DECODE`, `LK_MF_ENCODER_ADAPTER`, `LK_MF_ALLOW_AMD`,
  `LK_FFI_LOG_LEVEL` and `LK_FFI_WORKER_THREADS` are unchanged.
- **Test harnesses** behind webrtc-sys's `harness` feature (define `ROOTAPP_HARNESS`), off by
  default: `adm_proxy_harness.cpp` (AdmProxy/voice/engine/monitor harnesses, `FakePlatformAdm`,
  `HarnessTransport`), `root_screen_audio_harness.cpp` (the fake tone backend, `ScriptedBackend`, the
  pipe and capture harnesses), the "Test hooks" sections of `root_audio_processing.cpp` (processor and
  APM harnesses, `FakeApm`), `root_speech_detector.cpp` (the speech test signals and probes),
  `root_denoiser.cpp` (`RootFakeFxPipeline`, the denoiser harness), `root_echo_model.cpp` (echo
  harness and probes) and `root_audio_mixer.cpp` (mixer harness), the `*ForTest` members of
  `AdmProxy`, `RootAudioMixer`, `RootDenoiserSlot` and `RootApmDecorator`, the harness classes in the
  headers, and the harness halves of the cxx bridges (`#[cfg(feature = "harness")]` blocks and
  structs, which cxx-build evaluates from `CARGO_FEATURE_*`). The `root_audio_mixer` bridge is
  test-only and joins the bridge list only with the feature. Upstream's always-on `LIVEKIT_TEST`
  define (two serialisation test hooks) goes with it. A release library therefore no longer links
  the screen-audio tone backend and its `g_fake_backend` switch, and no longer exports the 33 cxx
  glue functions of test-only shared structs (`RootDenoiserTest*`, `RootMixLevel`,
  `RootVoiceFakeCounts`, `RootControlState`, `RootMonitorStats`).
- Tests need no flag: webrtc-sys has a dev-dependency on itself with `harness` and
  `mf-fault-injection`, and livekit-ffi one on webrtc-sys with `harness`. With resolver 2 a
  dev-dependency's features only apply to builds of tests, examples and benches, so
  `cargo build -p livekit-ffi` (the build scripts and the release workflow) never enables either;
  `cargo tree -p livekit-ffi -e no-dev,features -i webrtc-sys` shows only `default`.

libwebrtc's log floor: see "Logging" (the sink's minimum is now the global minimum; at the default
`warn` no `LS_INFO` line is formatted).

A/B switches removed, and the native defaults are the desktop's choices (its
`PlatformControlState`/`PlatformGainEngine`/`PlatformSpeechDetection`/`PlatformEchoModel`):

- Speech detection RING is gone. The wire value 1 is applied as GATE (livekit-ffi, livekit,
  the controller and `rootapp_voice_dsp::SpeechDetection::from_wire`); other unknown values stay off.
- A platform audio session (the first handle's `reset_root_audio(false)`) starts with AGC2, GATE
  and the echo model's AUTO. Outside a session (before the first handle and after the last one
  closes) native holds LEGACY, OFF and OFF as before, so the managed engine and an idle library run
  no speech detector, build no AGC2 and poll no echo model. LEGACY stays selectable (live tuning),
  as do OFF detection (the desktop sends it for an open mic) and the echo model's OFF and ON; the
  shadow legacy gain and the tuning stats 125-131 and 140-154 are kept.
- The desktop sends AGC2, GATE (OFF for an open mic) and AUTO on calls, so calls are unchanged. Its
  mic test sends no `echo_model`; it now runs in AUTO (it ran OFF). The monitor is mixed outside the
  echo canceller's reference, so the model sees no far end there and its output is AEC3's.
- The other defaults already matched what the desktop sends: auto sensitivity minimum -54 dBFS,
  voice gain 24 dB / -25 dBFS / 3 and 2 dB/s / 4000 ms / 0.5, AGC2 5 dB / 30 dB / 6 dB/s / -50
  dBFS, look-ahead 40 ms (never sent), gate threshold 0.025, auto gain and auto sensitivity on.
  `denoise_power` stays 0.75 natively (the desktop's setting defaults to 0.6 and is always sent with
  a denoiser).
- `LK_MF_D3D11_SHARING` and `LK_MF_D3D11_DRIVER_THREADING` are gone (see "Sharing the D3D11 device
  with the host").

Repeating logs in the video and capture paths log the first occurrence, then at most one every 10 s
with ` (N more since the last report)`: the MF encoder's pending upload copy, MFT-dropped frames,
output without frame metadata and `SetRates` while uninitialized or with an invalid frame rate;
libwebrtc's unconvertible remote video frames; livekit-ffi's failed capture callbacks.

Tests: `cargo test -p webrtc-sys --lib --release` (the harness tests, `webrtc::tests` for the log
floor, `mf_device` with fault injection), `cargo test -p rootapp-voice-dsp --release` (golden vectors
unchanged), `cargo test -p livekit-ffi --lib --release`, `cargo test -p libwebrtc --lib --release
video_stream`, `cargo test -p livekit --lib --release platform_audio`.

## macOS: the ADM's CoreAudio listeners on the device thread (`rootapp/mac-adm-listener-thread`)

Two Mac staging aborts on the `audio-device` thread were the same race: 10/16 (0.10.138, hw.14,
an AirPods swap) and 10/20 (0.10.145, hw.17, dylib UUID `C733994A-6CA0-3981-9AF3-DD0F6CE58DFF`).
Both were `platform_thread.cc:181` `RTC_CHECK(0 == pthread_join(*handle_, nullptr))`. They were
symbolicated by hand against the stripped release dylibs: the UUID matched, then the file, line and
check strings beside each call site were read with rustup's `llvm-objdump` (not `--macho`, which
ignores the address range). In these dylibs `__TEXT`'s vmaddr is 0, so a string's address is its
file offset.

- 10/20: CoreAudio's listener queue (`HALC_ProxyNotification Call Listener Queue`, under
  `HALSystem::ObjectsPublishedAndDied`) was in `AudioDeviceMac::HandleDeviceChange`. The open output
  device was not alive, so it ran `StopPlayout`, blocked in `render_worker_thread_.Finalize()`'s
  `pthread_join`. The device thread was in `RequestReconcile`'s task: `ReconcilePlayoutLocked` (the
  stale/unwanted stop), `StopDeviceLocked`, `AudioDeviceModuleImpl::StopPlayout`,
  `AudioDeviceMac::StopPlayout`, and a second `pthread_join` of the same thread failed.
- 10/16: the listener queue's "Default audio output device has changed" restart had just joined the
  render worker inside `StopPlayout`. The device thread's `StartPlayout` then move-assigned
  `render_worker_thread_` and joined the already-joined thread. The hw.16 change (a failed start is
  stopped at once, f1a54064) is harmless but was not the cause: a join on a live thread waits, it
  does not fail.

webrtc-sdk m150's `AudioDeviceMac` (the pinned prebuilt, not upstream libwebrtc) restarts devices from
its CoreAudio listener:
- `kAudioHardwarePropertyDevices` (`HandleDeviceChange`): for an open direction whose device is not
  alive it stops, and if it was running it runs `SetPlayoutDevice(0)` / `SetRecordingDevice(0)` and
  then Init and Start.
- The default output/input changed: while it plays (records) index 0, it runs Stop, Init and Start.

Its `StopPlayout`/`StopRecording` release `mutex_` around the 2 s stop-event wait and around the
worker join, and `PlatformThread` is not thread-safe. So any two threads in Stop or Start of one
direction can join one worker twice. `AdmProxy` does Stop/Init/Start on its device thread for the
reconcile, the format watch's reopen, `RestartDevices` and `SelectDevices`, often triggered by the
same device change.

The prebuilt cannot be patched, so `AdmListenerRouter` (`webrtc-sys/src/adm_listener_router.cpp`,
`include/livekit/adm_listener_router.h`) moves the ADM's listener onto the device thread:
- On macOS the library defines `AudioObjectAddPropertyListener` and
  `AudioObjectRemovePropertyListener`. The statically linked libwebrtc's calls bind to these. CoreAudio's
  own are reached through `dlopen`/`dlsym` of `CoreAudio.framework`.
- A listener is routed when it is added on a thread with a router or with the proc and client of a
  listener routed earlier. Threads with a router are the device thread (all of it) and the platform
  ADM's `Init` in the `AdmProxy` constructor (an `AdmListenerScope`; the device thread is now created
  before that `Init`). A routed listener is registered with CoreAudio as a trampoline whose client
  data is an id. Every other listener goes to CoreAudio unchanged.
- The trampoline copies the addresses and posts the notification to that `AdmProxy`'s device thread.
  It takes only the registry and router locks, briefly, and never waits for the device thread.
- On the device thread, `RunListenerNotification` runs the ADM's own listener under `mutex_`, in turn
  with every other device operation, then requests a reconcile. The ADM may have stopped, reopened
  or switched a direction there: a direction its handler closed and could not reopen is reopened on
  the desired selection, and the published state is current.
- Remove: a routed registration is erased and marked removed before CoreAudio removes its
  trampoline. A notification already posted for it is dropped when it runs. The registry lock is
  never held across a CoreAudio call, so a remove that waits for a running trampoline cannot
  deadlock. CoreAudio's thread no longer enters `AudioDeviceMac`, so the 10/16 report's listener
  waiting on the ADM's mutex is gone too.
- A second add of the same listener (object, address, proc, client) is kept once. A leftover with
  that key from a detached router is replaced.
- After `AdmProxy`'s shutdown no listener runs (`terminated_`). The destructor detaches the router
  before stopping the thread, so nothing is posted after.

What the ADM does on a device change (switch to the default when its device goes away, follow the
default) is unchanged; only the thread is new. No FFI request, proto field, stat or caps bit changed.
Since hw.22 the ADM no longer gets the device-list and default-device notifications at all; `AdmProxy`
follows those changes itself (see "macOS: one device for both directions").

Detection:
- On macOS `AdmProxy` logs a warning when the platform ADM's `Init` registered no routed listener (the
  override did not bind).
- `build-rootapp-macos.sh` fails a dylib that still imports `_AudioObjectAddPropertyListener` or
  `_AudioObjectRemovePropertyListener` (`nm -u`).

Tests (`cargo test -p webrtc-sys --lib --release -- adm_`). The harness's fake platform ADM registers
its listeners like `AudioDeviceMac` (a devices listener in `Init`, a format listener in
`InitPlayout`), through a `FakeCoreAudio` that goes through the router as the macOS definitions do.
On a devices notification it stops and reopens playout on device 0, and it counts overlapping device
calls. The tests:
- `adm_proxy_runs_the_platform_adms_listeners_on_the_device_thread`
- `adm_proxy_holds_a_listener_notification_until_the_device_operation_in_progress_ends` (the race: a
  notification while the device thread is inside `StartPlayout`)
- `adm_proxy_reopens_what_the_platform_adms_listener_closed`
- `adm_proxy_drops_a_notification_whose_listener_was_removed_before_it_ran`
- `adm_proxy_runs_no_listener_after_shutdown`
- `adm_listener_router_routes_only_the_platform_adms_listeners`

With routing switched off all six fail; in the race test the listener ran on the notification thread
while `StartPlayout` was in progress. hw.22 moved these tests onto the format listener (the one
notification the ADM still gets besides processor overloads), and
`adm_proxy_keeps_device_changes_from_the_platform_adms_listener` replaced
`adm_proxy_reopens_what_the_platform_adms_listener_closed`.

Not verified:
- The macOS link: whether Apple's linker binds libwebrtc's references to these definitions. The build
  script's check answers it.
- A run on a Mac.

The macOS branch of `adm_listener_router.cpp` passed a clang 19 `-fsyntax-only -Wall -Wextra` check
with `WEBRTC_MAC` against stub `CoreAudio/CoreAudio.h` and `pthread_spis.h` headers that mirror
Apple's declarations.

To verify on a Mac, platform engine, `ROOTAPP_CALL_DIAG=1`, in a call:
- AirPods disconnected and reconnected.
- The default output changed in Sound settings.
- Wired headphones unplugged and plugged back in.
- A USB mic unplugged.

Each should end with audio on the expected device and no abort. The log should show no "listeners
are not routed" warning.

## hw.20 (2026-10-20)

hw.19's production cleanup plus the macOS ADM's CoreAudio listeners on the device thread (see
above). Caps stay 32767.

## Capture I420 buffer pool (`rootapp/i420-pool`)

`CaptureVideoFrame` used to convert every frame into a new I420 buffer. Blocks over about 1 MB come
straight from `VirtualAlloc`, so each frame paid first-touch page faults on fresh pages: 340, 760
and 1350 of them at 720p, 1080p and 1440p: 62%, 36% and 35% of the request with libyuv's AVX2
rows (measured below).

Each FFI video source now owns an `I420BufferPool` (`video_frame::native` in libwebrtc), capped at
8 buffers. It wraps libwebrtc's `webrtc::VideoFrameBufferPool` (`zero_initialize = false`) behind a
`webrtc::Mutex`, in `webrtc-sys/src/video_frame_buffer.cpp`.
- A buffer is handed out again only when the pool holds its only reference (`HasOneRef`). A frame
  still queued for or inside the encoder, VideoStreamEncoder's pending frame, the screenshare
  cadence adapter's repeat frame, or one an FFI sink or client handle holds is never overwritten.
- When all 8 are in use, `create` allocates a buffer outside the pool; it never waits or drops.
- A new size purges the old buffers at the next capture. Buffers still held stay valid until
  released.
- The pool only grows to the number of frames in flight at once (one in the sink benchmark below).
  It keeps them until the source is disposed or the size changes: 8 buffers at 1440p is 44 MB at
  worst.

Why 8: the frame being converted; the tasks libwebrtc's frame cadence adapter and
VideoStreamEncoder post for each frame (about two each when the encoder queue lags); the frame
inside `Encode` or VideoStreamEncoder's pending frame; and two for an FFI preview sink (its
one-frame queue and the frame the client holds). The MF encoder copies its input into its own
sample (`CreateInputSample`/`CreateD3DInputSample`), and simulcast layers are scaled into their
own buffers, so neither holds a capture buffer after `Encode` returns. Past 8, frames fall back to
plain allocation, as before.

Covered: the RGBA, ABGR, ARGB, BGRA and RGB24 conversions, and I420/I420A copies whose strides
are already packed. Padded I420 keeps its strides and a plain buffer; I422, I444, I010, NV12 and
the receive side are unchanged. Every conversion writes each byte, including the odd-size chroma
edge, so recycled contents never leak.

Measured on a Ryzen 9 9950X3D: `livekit_ffi_request(CaptureVideoFrame)` with BGRA, release DLLs
from `build-rootapp-windows.ps1`, medians of two interleaved runs of 300 frames, in ms. "Sink" is a
local `VideoStream` (I420) on the source's track, its frames released 2 ms later.

| | 1280x720 | 1920x1080 | 2560x1440 |
|---|---|---|---|
| before, no sink | 0.222 | 0.618 | 1.169 |
| pool, no sink | 0.085 | 0.395 | 0.756 |
| before, sink | 0.323 | 0.661 | 1.121 |
| pool, sink | 0.205 | 0.476 | 0.842 |

Without a sink the request is now the AVX2 conversion alone (0.083 / 0.403 / 0.751 ms into a warm
buffer). The sink received 330 frames from one pooled buffer, against 19-29 distinct addresses
before.

Tests: `cargo test -p libwebrtc --lib --release video_frame` (reuse, a buffer held by a
`VideoFrame` or by a real local `NativeVideoStream` sink is skipped, the cap, a size change) and
`cargo test -p livekit-ffi --lib --release colorcvt` (pooled output byte-identical to the
unpooled path for all five packed types at 641x359, 1280x720, 3x3 and 1x1, into a reused buffer
pre-filled with other bytes).

## macOS: one device for both directions (`rootapp/mac-same-device`, hw.22)

Mac report: a headset unplugged and plugged back in mid-call, after which nobody was heard. Capture
worked, `Playing()` was true, and the render callbacks had stopped (Sentry: `playing=True, init/start
0/0`, render callbacks frozen). It happens only on devices that are one CoreAudio device for both
directions (USB and wired headsets, docks), not on AirPods, whose `:input` and `:output` are two
devices.

webrtc-sdk m150's `AudioDeviceMac` (89d790b, `audio_device_mac.cc`) runs both directions of one device
in one IOProc (`deviceIOProc`), and gives the microphone an IOProc of its own (`inDeviceIOProc`) while
playout is on another device. It decides this at each Init from what is open at that moment:
- `_twoDevices` is recomputed whenever a direction initializes its device (415-418, 471-474).
- InitRecording creates the microphone's own IOProc when `_twoDevices` is true. When it is false, it
  creates the shared IOProc, but only if playout is not initialized (1257-1263).
- InitPlayout creates the shared IOProc only when `_twoDevices` is true or the microphone is not
  initialized (1065-1068). StartPlayout starts it on the same condition, but sets `_playing` either way
  (1438-1442).
- Only `implDeviceIOProc`, the shared IOProc, renders (2259-2331).

So playout that moves onto the device of a microphone that has its own IOProc gets no IOProc, and
renders nothing while `Playing()` is true. That was the report: the mic went to the re-plugged headset
first (the desktop re-selects the mic first, and the ADM's own default-device listener restarts one
direction at a time in CoreAudio's order), then playout followed.

From there the state does not recover:
- StopRecording now takes the shared branch (1320) and waits 2 s for a stop signal only the missing
  IOProc sends (1354, 2288). It then stops a stale IOProc id, and the microphone's own IOProc runs
  until the process exits.
- StopPlayout also waits 2 s (1468).
- Every later playout-only reopen onto that device lands in the same state.

The reverse hazard: playout leaving the device whose shared IOProc a microphone uses creates its new
IOProc under the same `_deviceIOProcID` (1065). The old IOProc keeps running, no longer captures
(`_twoDevices` is now true), and both devices pull the render ring.

`AdmProxy` translates a UID to the current AudioObjectID at every open, so stale ids were not the
cause. Its reconcile could not see the state either, since the ADM reported initialized and playing.

### Playout opens before a microphone on its device

With a device probe (macOS: `CoreAudioDeviceIdentity`, `CoreAudioDeviceAlive` in `adm_proxy.cpp`),
`AdmProxy` records what each open direction opened on:
- The AudioObjectID and UID, identified right after its Init, from the GUID the selection resolved to,
  or from the default device at that moment.
- For the microphone, which IOProc the ADM gave it:
  - shared, when playout was open on its device;
  - its own, when playout was open elsewhere;
  - unknown, when playout was closed or a device could not be identified.

Before each reconcile pass (`StopForSameDeviceLocked`), if playout is about to open or reopen while a
microphone is open, `AdmProxy` checks whether the microphone's IOProc suits the device playout will open
on. It suits when the IOProc is shared and playout is on that device, or when it is the microphone's
own and playout is elsewhere. If it does not suit, the pass:
1. stops the microphone;
2. stops playout (if open);
3. opens playout;
4. opens the microphone.

The microphone then opens next to playout and always gets the right IOProc. This covers every path
that opens playout, because all of them go through the reconcile:
- `SelectDevices` (either direction, in one request or two, in either order);
- `RestartDevices`;
- the hold, the app and the monitor;
- the hw.13 format watch's reopen;
- the device changes and the render-stall guard below.

This is narrower than "reopen both whenever both resolve to one device" in two cases, on purpose. A
microphone that opens or moves onto the device where playout already runs shares playout's IOProc
correctly (1257-1263 with `_playIsInitialized`, 1298-1303), so playout is not restarted at every call's
publish on a USB headset. Playout reopening on the device whose IOProc a microphone shares (the format
watch's reopen) keeps that IOProc running; StopPlayout leaves it alone while recording (2276), so the
microphone is not restarted either. It is wider in two cases the "both on one device" rule misses:
- playout leaving the shared device (above);
- a microphone opened while playout was closed. That microphone got whichever IOProc the ADM's last
  output device suggested, so it is always reopened after playout.

The stop order is microphone first. Since the microphone's IOProc only ever opens next to a playout it
suits, `_twoDevices` at its stop is what it was at its open, and both orders stop cleanly without a
wait. The one exception is the silent state itself, where both directions are open on one device and
the microphone has its own IOProc (reached through a race, see the guard below). There StopRecording
would take the shared branch. So `AdmProxy`:
1. stops playout first (its 2 s wait cannot be avoided);
2. points the closed output at another device (`SetPlayoutDevice` plus `InitSpeaker`, which recomputes
   `_twoDevices` to true, 406-419);
3. stops the microphone, which now stops its own IOProc (`PointSpeakerAwayLocked`).

Without another output device it warns, and the microphone's IOProc may stay running, as before.

### AdmProxy follows device changes itself

`RunListenerNotification` keeps three notification types from the ADM's listener:
- `kAudioHardwarePropertyDevices`;
- `kAudioHardwarePropertyDefaultOutputDevice`;
- `kAudioHardwarePropertyDefaultInputDevice`.

The ADM only gets the rest: the stream format (still needed, see hw.13) and processor overloads.
`HandleDeviceChange` and `HandleDefault*DeviceChange` therefore never run, and with them the ADM's
own stop/restart threads (the 10/16 and 10/20 crash class). On the device thread,
`FollowDeviceChangesLocked` checks each open direction:
- **Gone:** the device is gone when CoreAudio no longer lists it or reports it not alive
  (`kAudioDevicePropertyDeviceIsAlive` 0, or a bad device or bad object error). The direction reopens.
  If a specific device was selected, that selection falls back to the system default, as the ADM did,
  until the selection changes or the device is listed again. It then reopens on the device, with no new
  selection needed.
- **Default moved:** a direction opened as the system default (or under the fallback) reopens when
  the default moves away from the device it opened.

Both go through the restart generations, so they reopen as above. The hw.13 format watch's saved
device is the one each reopen opened. The desktop re-selecting or restarting after these changes
still works the same way: a selection or restart is applied as before, usually as one more reopen.

### Render-stall guard

The device thread polls every 100 ms while platform playout plays. It reopens playout when all of these
hold:
- the render callback count has not moved for 500 ms;
- capture callbacks went on, at least 10 since then and some since the last poll;
- no playout format change is settling.

Recoveries are at most one every 5 s. When both directions are open on one device, the microphone is
reopened with playout (forced coupled). macOS only.

### Stats (155-169) and logs

`RootAudioStats` gains these fields. They are optional, and present only where native follows device
changes (macOS); absent elsewhere and from older libraries. livekit: `RootAudioStats::device_changes`
(`DeviceChangeStats`).

| # | Field | |
|---|---|---|
| 155, 156 | `playout_device_uid`, `recording_device_uid` | CoreAudio UID of each open direction's device; empty while closed |
| 157 | `same_device` | both open on one device |
| 158-162 | `device_list_notifications`, `default_output_notifications`, `default_input_notifications`, `stream_format_notifications`, `other_device_notifications` | the ADM's CoreAudio notifications by property |
| 163-167 | `user_select_reopens`, `default_change_reopens`, `device_lost_reopens`, `format_change_reopens`, `render_stall_reopens` | directions stopped and opened again, by reason; `RestartDevices` counts as a user select, and a return from the fallback as device lost |
| 168 | `coupled_reopens` | passes that stopped the microphone (and playout on its device) so that playout opened first |
| 169 | `render_stall_recoveries` | |

All counters are cumulative for the engine's life. No new control, and caps stay 32767.

Logs:
- `AdmProxy: stopping the microphone on device <id> so that playout opens first`
- `playout on device <id> has no IOProc; stopping it, then the microphone`
- `<direction> device <id> went away; reopening`
- `the default <direction> device is <id>, not <id>; reopening`
- `the selected <direction> device went away; opening the system default`
- `playout on device <id> rendered nothing for <n> ms while capture went on; reopening`
- `<direction> opened on device <id> (<uid>)` (info)

### Tests

Windows harness, `cargo test -p webrtc-sys --lib --release --features harness`.

`FakeMacAdm` in `adm_proxy_harness.cpp` models `AudioDeviceMac`'s rules over a fake HAL:
- `_twoDevices`, the speaker and microphone mixers;
- `_deviceIOProcID` and `_inDeviceIOProcID`, including stale ids;
- the stop branches, with a 2 s wait counted when no IOProc answers;
- IOProcs that run only on a live device;
- which IOProc renders and captures.

`mac_rules(probe)` switches the fake to it; `probe` gives the proxy a device probe over the fake's
devices.

| Test | What it checks |
|---|---|
| `adm_proxy_mac_playout_joining_the_microphones_device_renders` | The report's sequence (mic to the headset, then playout): the coupled reopen, one shared IOProc, no wait, stats, nothing running after the call. Run against hw.21's `AdmProxy` first, it failed with 0 render callbacks for 50 capture callbacks. |
| `adm_proxy_mac_fake_goes_silent_when_each_direction_reopens_alone` | Without the probe (the earlier behaviour), the fake reproduces the report: no render, `Playing()` true, then a 2 s StopRecording wait, a stale IOProc stop, and the microphone's IOProc left running. |
| `adm_proxy_mac_follows_a_new_default_device_in_either_notification_order` | A re-plugged headset becomes the default, with input or output notified first, in one pass or two. |
| `adm_proxy_mac_follows_the_default_when_the_default_device_goes_away` | Unplugging the default headset moves both directions to the new defaults. |
| `adm_proxy_mac_falls_back_to_the_defaults_while_the_selected_device_is_gone` | An unplugged selected headset falls back to the defaults and returns when the headset is back. |
| `adm_proxy_mac_render_stall_guard_reopens_silent_playout` | A default change that races the playout open leaves playout silent. The guard recovers it in the playout-first order and leaks no IOProc. It does not fire without capture. |
| `adm_proxy_mac_reopens_a_microphone_opened_before_playout` | A microphone opened before playout is reopened, with its own IOProc and with a shared one. |
| `adm_proxy_mac_playout_leaving_the_shared_device_keeps_the_microphone` | Playout leaving the shared device keeps the microphone capturing. |
| `adm_proxy_mac_leaves_a_suited_direction_alone` | The format watch's reopen under a shared IOProc, and next to the microphone's own IOProc on another device, leaves the microphone alone. |
| `adm_proxy_keeps_device_changes_from_the_platform_adms_listener` | Device notifications are no longer forwarded to the ADM's listener. |

With the coupling switched off, five of the Mac tests fail. The other suites:
- `cargo test -p livekit --lib --release platform_audio` (`root_audio_stats_fill_the_device_changes_only_where_native_follows_them`);
- `cargo test -p livekit-ffi --lib --release` (`rootapp_audio_device_change_stats_convert_and_stay_absent_where_not_followed`).

### Not verified until a Mac build or a live test

- The macOS branches compile here only as a syntax check:
  - files: `adm_proxy.cpp` (the CoreAudio probe, the selector static_asserts), `adm_listener_router.cpp`,
    `audio_device_controller.cpp` and `adm_proxy_harness.cpp`;
  - tool: clang-cl 23 `/Zs /W4` with `WEBRTC_POSIX` and `WEBRTC_MAC`;
  - stubs: CoreAudio and CoreFoundation, with SDK 15's types and values.

  Not checked: AppleClang, Apple's libc++ and SDK, and linking.
- The model of `AudioDeviceMac` is from the source. Real IOProc timing, and a dead device's stops
  (each waits 2 s, as the ADM's own handler did), are only seen on a Mac.
- The render-stall guard's threshold against Bluetooth profile switches. A switch that stalls output
  over 500 ms while the built-in microphone captures reopens playout. Watch
  `render_stall_recoveries` on AirPods switches.

To verify on a Mac (platform engine, a USB or wired headset with a microphone, `ROOTAPP_CALL_DIAG=1`):
1. Join a call on the built-in devices with the headset unplugged.
2. Plug the headset in. Both directions should follow it, and the other side should still hear you
   and you them.
3. Unplug it and plug it back. Same result.
4. In stats: `same_device` true, both UIDs the headset's, `render_stall_recoveries` 0, and
   `coupled_reopens` above 0.
5. In the log: no 2 s "Timed out stopping" while the headset is connected.

## hw.22 (2026-10-21)

The macOS same-device fix: playout opens before a microphone on its device, native follows device
changes itself, and a render-stall guard (stats 155-169, no control, caps 32767). See "macOS: one
device for both directions". Carries hw.21's capture I420 buffer pool.

## AGC2 ahead of the denoiser (`rootapp/agc2-before-denoiser`)

Users heard voices on native calls as lower quality, with small artifacts, against the DotNetBrowser
path. Desktop Chrome runs AGC2 too (adaptive digital, libwebrtc's defaults), but inside its APM,
before the page's Effects SDK; hw.14 put it after the SDK. The Effects SDK depends on the absolute
level it gets: it erases more of a quiet voice, clips word starts after pauses, and leaves a chirpy
residual that a gain after it lifts by up to 30 dB. Offline A/B of 2026-10-21 (real Effects SDK 1.7.2
at 0.60, gd_probe's AGC2; Jesse's headset capture, native order -> Chrome order): speech frames erased
13.5 -> 4.2 %, the first 200 ms after a pause 14 -> 0.9 dB under the rest, audible musical blips in
pauses 4.1 -> 0.8 /s, DNSMOS OVRL 2.56 -> 2.69 (worst 10 % 1.88 -> 2.08). A -48 dBFS LibriVox talker:
blips 30 -> 1.2 /s, pause noise 38 -> 55 dB under the speech. Cost: on Jesse's room, about 9 dB more
(smoother) hiss in pauses, which the gate mostly removes. AGC2's 30 dB maximum never bound.

Pipeline (gain_engine AGC2): APM -> pre_denoise -> speech detector -> AGC2 -> `pre_denoise_gain` ->
Effects SDK -> aligner (`post_denoise_align`) -> `post_gain`. The legacy engine keeps hw.14's order.
- The speech detector and the digital-silence decision still see the frame before the gain.
- `DspChain::pre_denoise_gain(frame, gain_db)` copies the gained frame into what a frame without
  denoiser output passes (Missed, NotInUse, a bypassed pipeline), so the level never drops by the gain
  on those frames.
- `post_denoise_align` reports the denoised level and `pre_gain_rms` divided by that gain, and
  `post_gain` takes it as the External gain the frame carries (the `gain_db` it is handed is ignored),
  de-gains the copy the chain's voice gain measures, and judges NOISE against the input's floor before
  the denoiser. Auto sensitivity, the erased-speech count and every reported level stay before the
  gain, as before.
- The SDK's output lags its input by its own latency (about 40 ms) that the aligner cannot see (the
  SDK returns full frames from the first call), so the de-gain uses the current frame's gain: off by at
  most AGC2's gain change over that latency (6 dB/s: 0.24 dB; the 20 dB/s maximum: 0.8 dB). Turning
  AGC2 off mid-stream leaves about 40 ms of gained output measured as ungained.

No control, stat or capability change (caps 32767); proto comments only. Golden vectors unchanged
(legacy order bit for bit). Tests: the crate (`a_gain_before_the_denoiser_stays_on_every_frame_and_the_levels_stay_before_it`,
`auto_sensitivity_learns_the_same_with_the_gain_before_or_after_the_denoiser`,
`speech_the_denoiser_erases_is_counted_before_a_gain_ahead_of_it`,
`pre_denoise_gain_needs_its_pre_denoise_and_lasts_one_frame`; the no-alloc and no-panic runs);
`agc2_tests` (`root_capture_processor_agc2_runs_ahead_of_the_denoiser`: a -100 dBFS tone a rounding
fake erases at its own level survives with AGC2 and not with the legacy engine;
`..._frames_the_denoiser_skips_keep_the_gain`: missed and rate-bypassed frames within 0.3 dB of
denoised ones; `..._ahead_of_the_denoiser_learns_the_room_before_the_gain`: auto sensitivity's floor
-80 dBFS at 25 dB of gain, the gate never opens).

## Windows playout the ADM cannot open (`rootapp/hw24-fixes`)

Reports from 0.9.152 (Windows 10 19045 and Insider build 26300): platform playout never opened on the
system default output (`playout init/start -1/1`, "device init failed (-1)" on every restart), while
the desktop's probe of the same device opened.

The platform ADM is webrtc-89d790b's `AudioDeviceWindowsCore` (the prebuilt has no
`CreateWindowsCoreAudioAudioDeviceModule`). Its `InitPlayout` searches 16-bit PCM at 48000, 44100,
16000, 96000, 32000 and 8000 Hz in two then one channel (`IsFormatSupported`, shared mode), and calls
`IAudioClient::Initialize` (shared, `AUDCLNT_STREAMFLAGS_EVENTCALLBACK`, no
`AUTOCONVERTPCM`) with the format it found, or with the last one it tried when none matched. Without
conversion the audio engine accepts only its own mix format's rate and channel count, so the open
fails with `AUDCLNT_E_UNSUPPORTED_FORMAT` on a device whose mix format has more than two channels (5.1
or 7.1, which surround gaming headsets and their virtual devices use) or another rate (88.2, 176.4 or
192 kHz, 24 kHz). The ADM's `_TraceCOMError` line is the only place its HRESULT appears. The desktop's
probe opens with `AutoConvertPcm | SrcDefaultQuality`, which inserts the engine's channel matrixer and
resampler, so it opens on the same device. Nothing here is specific to build 26300.

`AdmProxy` (Windows):
- After a failed `InitPlayout` it repeats the ADM's open on the same endpoint (the selected GUID, or
  the console default) with a client it never starts (`DiagnoseWasapiPlayout`,
  `root_wasapi_playout.cpp`): the format search, then `Initialize`, `SetEventHandle` and
  `GetService`. It reports the step that failed, its HRESULT, the format asked for (and the closest
  match when none was supported), the mix format, and whether a client opens with the engine's
  conversion. For example: `Initialize 0x88890008 AUDCLNT_E_UNSUPPORTED_FORMAT, asking 8000 Hz 1 ch
  16-bit PCM (no 16-bit PCM format of the ADM's list is supported; closest 48000 Hz 8 ch 32-bit
  float); mix format 48000 Hz 8 ch 32-bit float (extensible, mask 0x63F); opens with the engine's
  conversion`.
- Where the converted client opens, playout runs on the proxy's own stream instead
  (`AdmPlayoutFallback`, `CreateWasapiPlayoutFallback`): shared, event driven, 48 kHz int16 in
  libwebrtc's channel count, `AUTOCONVERTPCM | SRC_DEFAULT_QUALITY | NOPERSIST`, a 30 ms buffer
  filled 10 ms at a time through the transport tap (render callbacks, the monitor and the guards work
  as on the ADM) on an MMCSS "Pro Audio" thread. Its delay (padding plus stream latency) feeds
  `PlayoutDelay`. Stops and failed starts close it; a stop that cannot join its thread within 2 s is
  retried a second later, as the ADM's.
- Every open tries the ADM first, so a device whose mix format changes back opens natively again.
- The open reports success when the fallback opened. The text reaches the request outcome
  (`AudioDeviceOutcome::init_detail`; livekit appends it to "device init failed (n): ", so a failed
  selection or restart carries it), the log (`AdmProxy: InitPlayout failed: -1; <text>`), and
  `RootAudioStats` 170-172:

| # | Field | |
|---|---|---|
| 170 | `playout_fallback` | playout runs on the fallback now |
| 171 | `playout_fallback_opens` | opens that took it, cumulative |
| 172 | `playout_init_detail` | the last playout open's diagnosis and what the fallback did; empty when the ADM opened |

The fields are present where native has a fallback (Windows), absent elsewhere and from older
libraries; livekit: `RootAudioStats::playout_fallback` (`PlayoutFallbackStats`). No control or
capability change (caps stay 32767). Recording has the same format search (a 4-channel microphone
array would fail the same way) and is not covered.

Tests (`cargo test -p webrtc-sys --lib --release`): `adm_proxy_playout_falls_back_where_the_platform_adm_cannot_open_it`
(the default and a selected device through the fallback, renders through the tap, libwebrtc's view,
the delay, back on the ADM once it opens), `adm_proxy_playout_fallback_follows_libwebrtcs_mono_and_closes_with_the_device`,
`adm_proxy_playout_failure_carries_the_diagnosis_when_nothing_opens` (no fallback where a converted
client does not open; a fallback that fails too), `adm_proxy_without_a_playout_fallback_reports_the_bare_failure`.
`wasapi_playout_diagnosis_and_fallback_on_the_default_output` runs the real diagnosis and the
fallback on the default output for 300 ms (silence) with `LK_HW_AUDIO_TESTS=1`; on Jesse's machine
(48 kHz stereo float): the repeated open succeeds, 30 renders, 20 ms delay, a 1440 frame buffer.
livekit `device_results_carry_why_the_init_failed`; livekit-ffi
`rootapp_audio_playout_fallback_stats_convert_and_stay_absent_without_a_fallback`.

Not verified: a device with a surround or 192 kHz mix format (none here, and changing the default
format is a system setting), and the fallback in a live call.

## PlatformAudio create apart from room connect (`rootapp/hw24-fixes`)

Report from 0.9.152 (a Windows machine with 10 recording and 15 playout devices): a
`NewPlatformAudio` had not returned after 10 s; a managed call started 11 s later connected only
0.2 s before that create returned, and an earlier platform call's join took 17 s.

Two things serialized the create with a connect:
- `LkRuntime::instance()` builds the `PeerConnectionFactory` under the global runtime lock, and room
  connect takes that lock first (`EngineInner::connect`). The factory created the platform ADM on the
  worker thread (`BlockingCall` in the `PeerConnectionFactory` constructor):
  `CreateAudioDeviceModule(kPlatformDefaultAudio)` runs `AudioDeviceWindowsCore::CoreAudioIsSupported`,
  which builds, `Init`s and terminates a throwaway ADM, then `AdmProxy` `Init`s the real one. Each
  `Init` is `_EnumerateEndpointDevicesAll` for both directions (webrtc-89d790b, disassembled): every
  active, disabled and unplugged endpoint, with `GetId`, the property store, `GetState` and an
  `IAudioEndpointVolume` activation with its range and step queries. So four full endpoint sweeps ran
  under the lock a connect waits for. The runtime is a weak reference, so this happens again whenever
  no room, track, source or PlatformAudio keeps one alive.
- Whatever the worker asked of `AdmProxy` while a device operation held the device thread waited for
  it: `RegisterAudioCallback` ran on the device thread (`RunDeviceTaskAndWait`), and
  `StereoPlayoutIsAvailable`, `StereoRecordingIsAvailable` and `SetStereoRecording` took the device
  mutex, which every device open and stop holds (a Windows `InitPlayout`, a 2 s `StopPlayout` join).
  A PeerConnection's first media engine reference initializes the voice engine on the worker
  (`adm_helpers::Init`, `RegisterAudioCallback`), so a connect waited for the device.

Which step took the 10 s on that machine is not known (here, the factory takes 347 ms and the platform
ADM 17 ms); the steps are now logged. The connect no longer waits for any of them:
- On desktop the factory's `AdmProxy` gets an `AdmLazyPlatform`: the platform ADM is created and
  initialized on the device thread by the first `AcquirePlatformAdm` (the first `PlatformAudio`), which
  waits for it; the worker and the runtime lock do not. A managed call never creates it. Mobile is
  unchanged. Created on the device thread, it is also released there (the Windows ADM uninitializes
  the COM apartment it entered). Before it exists the proxy lists no devices, as without a platform ADM.
- `RegisterAudioCallback` swaps the tap's transport atomically. On desktop the stereo queries answer
  what the platform ADM said when it was attached (stereo before), and `SetStereoRecording` is kept and
  applied before every recording open, as the playout channels are; a wish made before the platform
  ADM existed becomes what the ADM can do (the engine always asks for what is available).
- The FFI's `NewPlatformAudio` reads the device counts (`PlatformAudio::recording_device_count`,
  `playout_device_count`) instead of listing every name and ID (three enumerations per device on
  Windows, about 75 for 25 devices).

Logs (warnings when slow, info otherwise; the desktop forwards warnings):
- `AdmProxy: the platform ADM was created in <n> ms (creation <n> ms, Init and setup <n> ms)` (1 s)
- `PlatformAudio: created in <n> ms (handle lock, runtime, platform ADM, enable, device counts, audio
  processing <n> ms each): <r> recording / <p> playout devices` (2 s)
- `[PLATFORM_AUDIO_FFI] PlatformAudio created in <n> ms (PlatformAudio::new, device counts, session)` (2 s)
- `LkRuntime: PeerConnectionFactory created in <n> ms` (1 s)

Tests: `root_engine_a_peer_connection_never_waits_for_a_device_open` (the device thread blocked in
`StartPlayout` holding the device mutex; a PeerConnection's engine reference on the worker finishes at
once) and `root_engine_the_platform_adm_is_created_with_the_first_reference_without_holding_up_a_peer_connection`
(no creation for a managed call; with the creation held, the handle's acquire waits and a
PeerConnection does not; one creation for later handles). With `RegisterAudioCallback`'s old
device-thread wait put back, both fail. livekit `create_steps_log_each_steps_own_time`;
`test_audio_device_controller_bridge` (`LK_HW_AUDIO_TESTS=1`) now acquires before it lists.

## Track stream state (`rootapp/hw24-fixes`)

The desktop's video health telemetry (0.9.152) puts most remote-video freezes at `no_media`: no RTP
while the sender's connection quality is poor, for example a Watch Stream screen share with no
inbound stream for 12 s. The SFU tells the subscriber when it stops forwarding a track
(`SignalResponse.stream_state_update`: participant SID, track SID, `ACTIVE` or `PAUSED`), which its
allocator does when no layer fits (this subscriber's bandwidth estimate, or a publisher that sends
nothing usable). The Rust SDK dropped the message: `TrackInfo.stream_state` and the FFI
`TrackInfo.stream_state` existed (upstream too) but stayed `ACTIVE`. Upstream rust-sdks (main of
2026-10-21) still drops it; the JS SDK has `RoomEvent.TrackStreamStateChanged`.

- livekit: `SessionEvent` / `EngineEvent::StreamStateUpdate`, and the room stores each state on the
  subscribed track it names (`RemoteTrack::stream_state`) and, when it changed, sends
  `RoomEvent::TrackStreamStateChanged { participant, publication, stream_state }`. A track starts
  `Active`. A state for a publication whose track is not there yet (the SFU can pause a subscription
  before the media track arrives, the never-started case) waits on the publication and is applied
  when the track is set, reported right after `TrackSubscribed`; unsubscribing drops it (JS drops
  such states). States for unknown participants or publications are dropped.
- livekit-ffi: `RoomEvent.track_stream_state_changed = 1000` (`TrackStreamStateChanged`:
  `participant_identity`, `track_sid`, `state` as the FFI `StreamState`). RootApp cases of
  `RoomEvent` are numbered from 1000, as in `rootapp_audio.proto`, so upstream's next cases cannot
  collide. `TrackInfo.stream_state` (track info requests) now follows the SFU too.

Tests: livekit `stream_state_follows_the_sfu_and_reports_only_changes`,
`a_stream_state_before_the_track_waits_for_it_and_an_unsubscription_drops_it`; livekit-ffi
`track_stream_state_changed_is_a_room_event_of_its_own` (the case's field number on the wire). The
path from the signal client to the room event is not covered by a test here: that needs a LiveKit
server (an e2e test could pause a track with `SimulateScenario { subscriber_bandwidth }`, which the
Rust SDK does not send yet).

## Branch history (`rootapp/mf-hw-video` on top of `livekit-ffi/v0.12.76`)

| Commit | Change |
|---|---|
| 582a07e1 | MediaFoundation hardware H.264 encode/decode on Windows |
| 89d5a507 | MF backend fixes from building and running on Windows |
| a9415301 | `api/array_view.h` include; adds `build-rootapp-windows.ps1` |
| 10831058 | MF encode on D3D11-aware NVIDIA/AMD MFTs |
| 0aecbc65 | Encoder drains async output from the queue; re-init failures request an encoder switch |
| 5732272b | Software fallback for hardware encode/decode; Software senders never open a hardware encoder |
| c703d8d1 | Delay-load `mfplat.dll`; MF kill switches and encoder vendor gate/preference |
| be034b9f | Capture timestamps, bounded VBV, tracked input samples |
| 5db8c807 | Rate-control limits ordered around the mean, re-init on failed runtime VBV/max updates; decoder escalates repeated failures to software |
| 3e688c64 | FFI `simulcast_layers` (explicit lower layers, browser-style selection) and `force_stereo` (stereo Opus publish); `TF_NO_DTX` when `dtx=false`; subscriber answer keeps `stereo=1` |
| c640683f | win-arm64 LLVM cross build, first Linux build script, this document |
| 1d8b229e | Drop degenerate custom simulcast layers; pair arm64 cross headers with the CRT libs |
| e2a6b46b | win-arm64 clang-cl build disables libyuv NEON/SVE/SME like the official MSVC build |
| `rootapp/fx-xos` | macOS build script, Linux script for x64/arm64/cross with glibc and link checks, shared `build-info.json` and zips, Windows `-ToolsDir`/`-LlvmDir`/`-Protoc`/`-Zip`, `rootapp-ffi.yml` release workflow, cross-platform audit |
| `rootapp/fx-encoder` | MF encoder rebuilt when NVIDIA's latched VBV starves it, 3-frame VBV; one shared D3D11 device per adapter, non-blocking decoder readback; per-track start bitrate (screen shares up to 3 Mbps); NV12/I210/I410 buffer types no longer abort the process; x64 libyuv built with clang-cl |
| `rootapp/fx-review` | Decoder staging readback waits on an `ID3D11Fence` event instead of polling with `Sleep(1)` (polling fallback without WDDM 2.0); MF encoder teardown on a deferred release thread; Linux CI container installs `unzip` for setup-protoc; arm64 cross builds install `g++-aarch64-linux-gnu`; macOS `minos` check no longer exits awk early; libclang lookup resolves bare `CXX` names |
| `rootapp/fx3` | MF encoder upload ring never blocks in `Map` (fence-gated slots, grows to 5, then a fence wait outside the device lock); `D3D11GpuFence` in `mf_gpu_fence.h` with a WARP test; MF decoder teardown on the deferred release thread; a release run needs all six RIDs and refuses a tag, or a draft, that points at another commit |
| `rootapp/fx4-tokio` | FFI tokio runtime capped at 8 workers, `LK_FFI_WORKER_THREADS` override (see Tokio runtime size). Later: simulcast encodings lowest-first, periodic ALR probing, start bitrate through `SetBitrate` once per PeerConnection with a max that follows the published video (see Start bitrate), MF sessions counted until their delayed release with the D3D11 device kept alive that long, unpublished senders' streams left out of offers (see Unpublish and the publisher SDP), libwebrtc log levels and `LK_FFI_LOG_LEVEL` (see Logging), each video codec listed once by the codec factories |
| `rootapp/fx4-dev` | Windows exports `livekit_ffi_d3d11_acquire_shared_device` / `_release_shared_device`, so the desktop's screen capture runs on the codecs' shared D3D11 device; the shared device is created without the driver's internal threading optimizations (`LK_MF_D3D11_DRIVER_THREADING=on` restores them). See "Sharing the D3D11 device with the host" |
| `rootapp/native-platform-audio` | Native call audio, Phase 0: `rootapp_audio.proto`, `AdmProxy` on a device thread with stored selection, playout hold, restart and a transport tap, `RootAudioMixer`, explicit device-track options, external sources off the APM and mic, `livekit_ffi_rootapp_audio_caps`. Review fixes: one mix and one render/capture callback at a time, no second puller after a failed device stop, the app's start/stop apart from libwebrtc's, real-time rule counters in every build, AdmProxy tests over a fake platform ADM. Phase 1: mixer gains, ramps and effect bus, `RootApmDecorator` and `RootCaptureProcessor` (mute/PTT gate, input volume, levels, capture format events), track gains by SSRC, `RootAudioSetControl`, caps `0b100011111111`. Phase 1 review fixes: shared controls reset only by the first session and closed by the last, new streams held until their gain arrives, SSRCs resolved at subscribe, gate fades and one gate word, stale effects dropped, `unknown_track_handles`. Phase 2: `rootapp-voice-dsp` in the capture processor (voice gain, auto sensitivity, noise gate, startup hold, input volume, transmit gate, denoiser aligner; golden vectors), the Effects SDK slot with its `rootapp-effects` thread and native WebRTC NS fallback, speech-pause APM policy, speaking/denoiser events and stats 74-100, `livekit_ffi_rootapp_denoiser_notification_handler`, caps 3071, end-to-end voice tests. See "Native call audio" |

`rootapp/fx-session` adds: unpublishing a video track releases its encoder (see above);
received I420 frames whose planes are already packed are handed to the FFI handle without a
copy (other layouts still get the packed copy); stats deserialize libwebrtc's
`totalFreezesDuration`, `totalPausesDuration` and `scalabilityMode`; native audio captures of
one source run on one task, in submission order.
