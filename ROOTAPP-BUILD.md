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
The script compiles against the MSVC headers that belong to those libraries: a toolset's
`lib\arm64` pairs with that toolset's `include`, and an xwin splat's `crt\lib\aarch64` pairs
with its `crt\include` (plus its `sdk\include` and `sdk\lib` when present). For any other
folder it warns and falls back to the newest installed toolset's headers, which must be the
same MSVC version as the libraries or the link fails on `__std_*` / vcruntime symbols.
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
| `LK_MF_D3D11_SHARING=off\|user` | `off` gives every MF encoder and decoder its own D3D11 device again; `user` shares one device per adapter among encoders and another among decoders. The default shares one device per adapter between all of them. Read once per process. |

Fault injection, for testing recovery only:

| Variable | Effect |
|---|---|
| `LK_MF_FAULT_INIT=1` | MF H.264 `InitEncode` fails, which must fall back to software. |
| `LK_MF_FAULT_AFTER_FRAMES=N` | Encoder simulates device removal after N frames. |
| `LK_MF_MAX_SESSIONS=N` | Refuse to open more than N concurrent MF encoder sessions. |
| `LK_MF_FAULT_RUNTIME_RC=1` | Runtime VBV/max bitrate updates fail, as if the VBV were fixed at configuration (what NVIDIA does silently). |
| `LK_MF_FAULT_STRICT_RC=1` | Enforce strict rate-control ordering, like drivers that reject max < mean or an undersized VBV. |
| `LK_MF_FAULT_INIT_BPS=N` | Every MF encoder sizes its rate control for N bps at InitEncode, whatever its start bitrate. `30000` reproduces a simulcast top layer started at its placeholder minimum; `300000` a low start that the starvation re-init has to fix. |
| `LK_MF_FAULT_DECODE_AFTER_FRAMES=N` | Decoder fault after N frames. |
| `LK_MF_FAULT_DECODE_MODE=unlisted\|renegotiate` | Decoder fault kind. The default is `DXGI_ERROR_DEVICE_REMOVED`, `unlisted` is `E_FAIL`, and `renegotiate` is `MF_E_TRANSFORM_STREAM_CHANGE`. |

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

## Start bitrate

`x-google-start-bitrate` is munged per published video track into the m-section carrying
it (matched by `a=msid`): 0.9 × the sum of its encodings' max bitrates, capped at 1 Mbps
for cameras and 3 Mbps for screen shares. livekit-client leaves screen shares uncapped,
but Chromium writes its value only on the first H.264 payload type (42001f) while the SFU
answers with 42e01f, so the browser really starts at libwebrtc's default. 3 Mbps turns on
a 2K share's top layer (low layer 1.2 Mbps) at the first allocation without starting far
above typical uplinks.

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
| c640683f | win-arm64 LLVM cross build, Linux build script, this document |
| 1d8b229e | Degenerate custom simulcast layers dropped; arm64 cross headers paired with the CRT libs |
| e2a6b46b | win-arm64 clang-cl build disables libyuv NEON/SVE/SME like the official MSVC arm64 build |
| `rootapp/fx-encoder` | MF encoder rebuilt when NVIDIA's latched VBV starves it, 3-frame VBV; one shared D3D11 device per adapter, non-blocking decoder readback; per-track start bitrate (screen shares up to 3 Mbps); NV12/I210/I410 buffer types no longer abort the process; x64 libyuv built with clang-cl |
