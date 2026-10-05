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
| `LK_MF_MAX_SESSIONS=N` | Refuse to open more than N concurrent MF encoder sessions. |
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
the SDP) and then calls `RtpSender::release_video_encoder`, which makes libwebrtc recreate the
sender's `VideoSendStream` without a source. That destroys the encoder instances (MF sessions,
their D3D11 devices and NVIDIA driver threads); each unpublished video track keeps only an idle
stream (one `EncoderQueue` thread, stale outbound-rtp counters) until the PeerConnection closes.

libwebrtc's worker thread, which also delivers incoming audio, waits for the encoder's `Release()`
during that recreate. The MF encoder therefore hands the MFT shutdown and its device references to
one process-wide release thread (`webrtc-sys/src/mf/mf_deferred_release.h`) and returns at once;
a teardown on the worker cost 5-30 ms of concealed incoming audio per camera stop (0.1-0.5 s
without the shared D3D11 device). Opening an encoder session waits for pending teardowns first, so
a re-init never holds the old and the new NVENC session together and `LK_MF_MAX_SESSIONS` counts
stay exact. The encoder logs `MF encoder sessions open: N` on every open and close.

The MF decoder's `Release()` hands its MFT, staging texture and device reference to the same
thread. When a subscribed track goes away the worker waits for it too, and in the same process the
two teardowns used to run at once on the shared device. Because the queue is shared, opening an
encoder session also waits for a pending decoder teardown.

Stopping the transceiver instead is not compatible with LiveKit server 1.13.7:

- libwebrtc recycles a rejected m-section for the next transceiver under a new mid, but the
  server only treats m-sections after the last answered mid as new. A recycled slot that is not
  the last video section gets no simulcast rid mapping (`adding up track failed: duplicate
  layer`) and the track never reaches subscribers.
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

`x-google-start-bitrate` is munged per published video track into the m-section carrying
it (matched by `a=msid`): 0.9 × the sum of its encodings' max bitrates, capped at 1 Mbps
for cameras and 3 Mbps for screen shares. livekit-client leaves screen shares uncapped,
but Chromium writes its value only on the first H.264 payload type (42001f) while the SFU
answers with 42e01f, so the browser really starts at libwebrtc's default. 3 Mbps turns on
a 2K share's top layer (low layer 1.2 Mbps) at the first allocation without starting far
above typical uplinks.

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
| `rootapp/fx4-tokio` | FFI tokio runtime capped at 8 workers, `LK_FFI_WORKER_THREADS` override (see Tokio runtime size) |
| `rootapp/fx4-dev` | Windows exports `livekit_ffi_d3d11_acquire_shared_device` / `_release_shared_device`, so the desktop's screen capture runs on the codecs' shared D3D11 device; the shared device is created without the driver's internal threading optimizations (`LK_MF_D3D11_DRIVER_THREADING=on` restores them). See "Sharing the D3D11 device with the host" |

`rootapp/fx-session` adds: unpublishing a video track releases its encoder (see above);
received I420 frames whose planes are already packed are handed to the FFI handle without a
copy (other layouts still get the packed copy); stats deserialize libwebrtc's
`totalFreezesDuration`, `totalPausesDuration` and `scalabilityMode`; native audio captures of
one source run on one task, in submission order.
