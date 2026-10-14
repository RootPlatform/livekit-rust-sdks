# Golden vectors

`*.rdv` files written by the RootApp desktop's managed reference (`VoiceDspReference`, the production
`VoiceGain`, `AutoSensitivity`, `NoiseGate`, `GainRamp` and `DenoiseFrameAligner` in
`NativeMicrophoneCapture`'s order). `tests/golden.rs` runs every file through one `DspChain::new(48000)`
and compares the sent samples, the RMS levels, threshold, envelope, voice gain, block peak and flags.
Each file's header names its scenario, generator and desktop commit.

Regenerate from the desktop repo (never by hand):

```
set ROOTAPP_DSP_GOLDEN_DIR=<this folder>
dotnet Tests\RootApp.Client.Avalonia.Tests\bin\Debug\net10.0\RootApp.Client.Avalonia.Tests.dll -method RootApp.Client.Avalonia.Tests.Calls.VoiceDspGoldenVectors.WriteGoldenVectors -explicit only
```

Format (little-endian): `RDSPVEC1`, `u32` header length, `key=value` lines, then one `FRAM` record per
10 ms frame: rate, length, settings and commands (the first settings byte: bit 0 mic_muted, bit 1
capture_silent), the int16 input with injected non-finite values, the denoiser's output (kind, restart,
int16 or f32 samples), then the expected flags, sent samples (on stored frames) and seven f32 values. `ROOTAPP_DSP_GOLDEN_EXTRA` adds folders (replays of recordings, never
committed); `ROOTAPP_DSP_GOLDEN_REPORT=1` prints the largest deviation per field;
`ROOTAPP_DSP_GOLDEN_LOOSE=1` uses the looser tolerances meant for other platforms' math libraries.
