# RootApp livekit-ffi builds

Branch `rootapp/mf-hw-video` is `livekit-ffi/v0.12.76` (2d9f01ab) plus RootApp's
hardware codec and publish-option work. The desktop client loads the result through
the vendored C# binding in `external/LivekitRtc`. Output lands in `rootapp-dist/<rid>/native/`.

| RID | Artifact | Script | Hardware video |
|---|---|---|---|
| win-x64 | `livekit_ffi.dll` | `build-rootapp-windows.ps1` | MediaFoundation H.264 (NVIDIA/Intel/AMD MFTs) |
| win-arm64 | `livekit_ffi.dll` | `build-rootapp-windows.ps1 -Targets aarch64-pc-windows-msvc` | MediaFoundation H.264 |
| linux-x64 | `liblivekit_ffi.so` | `build-rootapp-linux.sh` | NVENC/NVDEC (CUDA), VAAPI |

## Windows

Requirements: Visual Studio or Build Tools with the x64 MSVC toolset and a Windows 10/11
SDK, rustup (the repo pins Rust 1.97.1 in `rust-toolchain.toml`), and
`%USERPROFILE%\dev-tools` holding `protoc\bin\protoc.exe` and an LLVM tree under `llvm\`
(libclang.dll, clang.exe, clang-cl.exe, lld-link.exe, llvm-lib.exe, `lib\clang\*`).
The libwebrtc prebuilt for the target is downloaded by `webrtc-sys` on first build.

```powershell
.\build-rootapp-windows.ps1                                   # win-x64 and win-arm64
.\build-rootapp-windows.ps1 -Targets x86_64-pc-windows-msvc
.\build-rootapp-windows.ps1 -Targets aarch64-pc-windows-msvc
.\build-rootapp-windows.ps1 -Targets aarch64-pc-windows-msvc -CompileOnly
```

Both targets link the CRT statically (`+crt-static` in `.cargo/config.toml`) and
delay-load `mfplat.dll` (`/DELAYLOAD:mfplat.dll` + `delayimp.lib`, set in
`livekit-ffi/build.rs`), so the DLL loads on Windows N/KN without the Media Feature Pack
and falls back to software codecs there.

### win-arm64 on an x64 machine

If an MSVC toolset has `bin\Hostx64\arm64\cl.exe`, cargo/cc-rs use it directly. Otherwise
the script cross-compiles with LLVM, setting only for that target:

| Variable | Value |
|---|---|
| `CC_aarch64_pc_windows_msvc`, `CXX_aarch64_pc_windows_msvc` | `clang-cl.exe` |
| `AR_aarch64_pc_windows_msvc` | `llvm-lib.exe` |
| `CFLAGS_/CXXFLAGS_aarch64_pc_windows_msvc` | `--target=aarch64-pc-windows-msvc` |
| `INCLUDE` | MSVC `include` + Windows SDK `ucrt;um;shared;winrt;cppwinrt` (headers are arch-neutral) |
| `CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_LINKER` | `lld-link.exe` |
| `CARGO_ENCODED_RUSTFLAGS` | `+crt-static` and `/LIBPATH:` for the ARM64 CRT and SDK `ucrt\arm64`, `um\arm64` |
| `PATH` | LLVM `bin` first, because `ring` switches to `clang` on Windows ARM64 |

cc-rs can't find an x64-hosted ARM64 `cl.exe`, so it doesn't inject a VS environment for
clang-cl. That's why `INCLUDE` is set explicitly. The variables are restored after
the arm64 build, so a following x64 build in the same run is unaffected.

The link still needs the **ARM64 MSVC runtime libraries** (`libcmt.lib`,
`libvcruntime.lib`, `libcpmt.lib`, `oldnames.lib`, `delayimp.lib`). They ship with the
VS component *MSVC ARM64/ARM64EC build tools*
(`Microsoft.VisualStudio.Component.VC.Tools.ARM64`). A VS install without it has only
the ASan libraries in `VC\Tools\MSVC\<ver>\lib\arm64`. If they live somewhere else,
for example in an `xwin splat` output (`crt\lib\aarch64`), pass `-Arm64CrtLibDir <dir>`
or set `LK_ARM64_CRT_LIB_DIR`. Without them the script stops before cargo with that message.
`-CompileOnly` still builds `target\aarch64-pc-windows-msvc\release\livekit_ffi.lib`
(staticlib, no link step), which proves every C/C++/Rust unit compiles for ARM64.

Check an arm64 DLL with `dumpbin` (under `VC\Tools\MSVC\<ver>\bin\Hostx64\x64`):

```powershell
dumpbin /headers livekit_ffi.dll | Select-String machine   # AA64 machine (ARM64)
dumpbin /imports livekit_ffi.dll                           # "delay load imports" lists mfplat.dll
```

## Linux (x64)

Run `./build-rootapp-linux.sh` natively on Ubuntu 22.04+ or a similar distro (no Docker).
It:

1. apt-installs the build deps (`build-essential pkg-config lld libssl-dev libglib2.0-dev
   libx11-dev libxext-dev libxfixes-dev libxdamage-dev libxrandr-dev libxcomposite-dev
   libgl1-mesa-dev libdrm-dev libgbm-dev libasound2-dev libpulse-dev libva-dev`, plus
   `nvidia-cuda-dev` if no `cuda.h` is found). `SKIP_APT=1` skips this.
2. Installs Rust 1.97.1 through rustup.
3. Uses `protoc` 3.15+ from PATH or `PROTOC`, otherwise downloads protoc `PROTOC_VERSION`
   (default 25.2, same as CI) to `~/.local`.
4. Installs clang 21.1.8 to `/opt/llvm-21.1.8` with `.github/scripts/install-clang.sh` unless
   `CC`/`CXX` are set. webrtc-sys requires clang 21+ because libwebrtc.a uses Chromium's
   hermetic libc++. `LIBCLANG_PATH` (yuv-sys bindgen) comes from the same tree.
5. Hardware codecs, which `webrtc-sys/build.rs` auto-detects:
   - NVIDIA NVENC/NVDEC need `$CUDA_HOME/include/cuda.h` (default `/usr/local/cuda`). If only
     Ubuntu's `/usr/include/cuda.h` exists, the script points `CUDA_HOME` at a shim directory
     holding just that header. `CUDA_HOME=/usr` would put `-I/usr/include` ahead of the
     hermetic libc++. `libcuda`, `libnvcuvid` and `libnvidia-encode` are dlopened at runtime.
   - VAAPI needs libva headers found via `pkg-config libva`. libva is dlopened at runtime.

   The script fails if either is missing. Set `ALLOW_NO_NVIDIA=1` or `ALLOW_NO_VAAPI=1` to
   build without that codec.
6. Runs `git submodule update --init --recursive`, then
   `cargo build --release -p livekit-ffi --target x86_64-unknown-linux-gnu`.
7. Copies the result to `rootapp-dist/linux-x64/native/liblivekit_ffi.so`.
8. Checks the copied `.so`. `strings` must find `libcuda.so.1`/`libnvcuvid.so.1`/
   `NvEncodeAPICreateInstance` (NVIDIA) and `libva.so.2`/`libva-drm.so.2` (VAAPI). It also
   prints the highest GLIBC symbol version and the NEEDED list.

A native build inherits the build host's glibc as its floor. Upstream's release `.so` is
built in `manylinux_2_28` for glibc 2.28. Build on the oldest distro you need to support.
`LK_DISABLE_NVDEC` (any value) turns NVDEC off at runtime.

## Runtime switches (Windows MediaFoundation)

The engine reads these through `GetEnvironmentVariableA`, so values the host sets after
the DLL loads still apply. Flags accept `1/true/yes/on`, and `LK_MF_ALLOW_AMD` also accepts
`0/false/no/off`.

| Variable | Effect |
|---|---|
| `LK_DISABLE_MF_ENCODE=1` | MF encoder factory reports no H.264 encoder. Encoding uses OpenH264. |
| `LK_DISABLE_MF_DECODE=1` | MF decoder factory reports no H.264 decoder. Decoding uses FFmpeg. |
| `LK_MF_ENCODER_ADAPTER=nvidia\|amd\|intel` | Try that vendor's encoder MFT first. |
| `LK_MF_ALLOW_AMD=0` | Skip AMD encoder MFTs (browser-parity workaround for AMD CBP black remote video). Allowed by default. |

Fault injection, for testing recovery only:

| Variable | Effect |
|---|---|
| `LK_MF_FAULT_INIT=1` | MF H.264 `InitEncode` fails, which must fall back to software. |
| `LK_MF_FAULT_AFTER_FRAMES=N` | Encoder simulates device removal after N frames. |
| `LK_MF_MAX_SESSIONS=N` | Refuse to open more than N concurrent MF encoder sessions. |
| `LK_MF_FAULT_RUNTIME_RC=1` | Runtime VBV/max bitrate updates fail, which forces an encoder re-init. |
| `LK_MF_FAULT_STRICT_RC=1` | Enforce strict rate-control ordering, like drivers that reject max < mean or an undersized VBV. |
| `LK_MF_FAULT_DECODE_AFTER_FRAMES=N` | Decoder fault after N frames. |
| `LK_MF_FAULT_DECODE_MODE=unlisted\|renegotiate` | Decoder fault kind. The default is `DXGI_ERROR_DEVICE_REMOVED`, `unlisted` is `E_FAIL`, and `renegotiate` is `MF_E_TRANSFORM_STREAM_CHANGE`. |

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
| (this) | win-arm64 LLVM cross build, Linux build script, this document |
