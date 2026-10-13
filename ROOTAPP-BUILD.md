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
| `LK_MF_D3D11_SHARING=off\|user` | `off` gives every MF encoder and decoder, and every host acquire of the shared device, its own D3D11 device again; `user` shares one device per adapter among encoders and the host and another among decoders. The default shares one device per adapter between all of them. Read once per process. |
| `LK_MF_D3D11_DRIVER_THREADING=on` | Create the shared D3D11 devices without `D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS`, as before `rootapp/fx4-dev` (see "Sharing the D3D11 device with the host"). Read once per process. |

Fault injection, for testing recovery only:

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
  `(M closing)`) on every open and final release, and `LK_MF_MAX_SESSIONS` counts closing
  sessions too.
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
- `LK_MF_D3D11_SHARING` applies: `user` puts the host on the encoders' device, `off` gives every
  acquire a device of its own.
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
  `LK_MF_D3D11_DRIVER_THREADING=on` drops the flag.
- Test: `cargo test -p webrtc-sys mf_device` covers the contract on a real adapter and skips on a
  machine without a hardware D3D11 video device. `LK_MF_FAULT_HOST_DEVICE_REMOVED=1` makes acquire
  report the device as removed.

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
SPEECH_DETECTION. This branch reports `0b1111111111111` (8191: bits 0-12) on 64-bit desktop,
`0b1110111111111` (7679, no DENOISER) on 32-bit desktop and 0 on mobile; builds up to hw.7 reported
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
  below -60 dBFS, plus the frame's voice gain; 5 blocks to open), voice gain on from 0 dB (it only boosts, up to
  18 dB), no denoiser. Every recording start silences its first 20 frames (200 ms, the startup
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
receiver). The publisher transport now keeps every audio send section's payload order and Opus fmtp
as its first answer gave them (`PeerTransport::pin_send_codecs`, a stereo track's first answer gets
`stereo=1` if the server left it out) and logs the first change it overrides per section. Device-free
tests:
`cargo test -p livekit --lib peer_transport` (string cases, plus a loopback pair whose answer gains
`maxaveragebitrate`: pinned the send codec stays, unpinned it changes). Against a local server
(`livekit-server --dev --bind 127.0.0.1`): `cargo test -p livekit --features __lk-e2e-test --test
screen_audio_codec_test -- --nocapture` (fails without the pin).

Tests that need no device: `cargo test -p webrtc-sys --lib -- adm_proxy root_
device_selection` (AdmProxy over a fake platform ADM that records its calls, the mixer, the capture
processor, the decorator over a fake and over the real builtin APM, the denoiser slot and its
control side, and `root_voice_*`: the whole voice path over one capture state with fake Effects SDK
pipelines and a worker thread), `cargo test -p rootapp-voice-dsp` (ported managed tests, 25 golden
vectors from the desktop's reference, no-allocation and no-panic sweeps; `cargo clippy -p
rootapp-voice-dsp --all-targets -- -D warnings`), `cargo test -p libwebrtc --lib imp::rtp_sender`,
`cargo test -p livekit platform_audio`, and `cargo test -p livekit-ffi --lib rootapp_audio` and
`platform_audio`. Run them with `--release`. Every test that creates the platform ADM
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
  The ring (`speaking`) is the level gate's speaking AND confirmed speech. In `Gate` mode:
  - an open the detector has not confirmed within 200 ms of frames under 0.3 is released over 50 ms
    (40 ms at full gain, then a 10 ms ramp; about 260 ms of it is sent) without the speaking hold;
  - when confirmed speech ends, `control` decides with that frame's level: input still held above
    the threshold is released the same way; a quiet frame only shortens the tail's release to 50 ms
    (a new onset ends it); a word that began after 30 ms of quiet, or rose 6 dB over what was
    rejected, keeps its own provisional window;
  - a released gate is held shut until a frame at p >= 0.3, an input 6 dB louder than the loudest
    input rejected since the last frame at 0.3, an onset (3 quiet frames, then 6 dB up) that stays
    above the threshold 3 frames, or 100 ms below the threshold. During an onset it opens on its
    first block above the threshold while the level gate alone would still be open
    (`GateControl::eager_open`). At or below -66 dBFS a frame counts as quiet and opens nothing
    whatever the threshold (a manual 0 still re-arms);
  - auto sensitivity follows the level gate alone (`NoiseGate::level_envelope`), so the threshold is
    Off's and Gate never sends a sample louder than Off.
  Push-to-talk, frames without a probability (off, other rates, NaN or negative) and unsupported
  frames leave the chain bit for bit as the level gate; mute does not bypass the speech gate (nothing
  is sent while muted anyway). A change of mode starts the verdict over.
- Control `RootAudioControl.speech_detection` (`RootAudioSpeechDetection` OFF/RING/GATE; unknown
  values off), stored in `RootCaptureState::speech_detection`, back to off when the first handle
  opens and when the last closes (`reset_root_audio`, as the other DSP settings).
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

### Echo model contract (`rootapp/echo-model`, stage 1: no native model yet)

DTLN-aec-128 (MIT, Westhausen) as AEC3's neural residual echo estimator in the platform engine's APM.
This stage only fixes the FFI contract; nothing native runs the model.

- `RootAudioControl.echo_model` (15, `RootAudioEchoModel` OFF/ON/AUTO; native OFF until set and at
  every session start and end; unknown values OFF) and `RootAudioStats` 114-124 (mode, state, reason,
  output device kind, swaps, inferences, inference mean/max since the previous read, the last window's
  p99, measured echo, kill switch). No new requests or events; nothing collides with the screen-audio
  branch (bit 13, requests 1012-1014, event 1001).
- Capability bit 14 `ECHO_MODEL` (`CAP_ECHO_MODEL`) is defined and **not reported**: caps stay 8191.
  `livekit::ECHO_MODEL_AVAILABLE` is false, so a control that sets `echo_model` is answered "not
  supported by this build" (the other fields are applied) and `RootAudioStats::echo_model` is `None`.
- Tests: `cargo test -p livekit-ffi --lib --release rootapp_audio` (`rootapp_audio_echo_model_*`,
  `rootapp_audio_control_maps_the_echo_model_*`) and `cargo test -p livekit --lib --release
  platform_audio` (`echo_model_*`).
- Building this worktree on Windows: the prebuilt's deepest abseil header
  (`absl/time/internal/cctz/include/cctz/civil_time_detail.h`) passes MAX_PATH under
  `livekit-rust-sdks-echo\target\release\build\scratch-*\out\livekit_webrtc\...`, and cl.exe fails
  with C1083. Point `LK_CUSTOM_WEBRTC` at a short junction to the extracted prebuilt
  (`win-x64-release` with `include`, `lib`, `webrtc.ninja`).

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
