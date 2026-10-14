#![forbid(unsafe_code)]

pub mod aligner;
pub mod auto_sensitivity;
mod chain;
pub mod gain_ramp;
pub mod noise_gate;
pub mod sanitize;
pub mod speech_gate;
pub mod transmit_gate;
pub mod voice_gain;

pub use chain::{DspChain, AUTO_MIN_OPEN_BLOCKS};
pub use speech_gate::SpeechDetection;

/// Managed `Math.Pow(10, x)`: libm's pow. The base goes through black_box because LLVM turns a
/// constant base of 10 into exp10 where libm has one (macOS, glibc), which can round 1 ulp away
/// from the pow .NET calls on the same platform.
pub(crate) fn pow10(x: f64) -> f64 {
    std::hint::black_box(10f64).powf(x)
}

/// Managed `Math.Clamp(double, min, max)` without the throw on inverted bounds.
pub(crate) fn clamp_f64(value: f64, min: f64, max: f64) -> f64 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

pub const MIN_RATE: u32 = 8_000;
pub const MAX_RATE: u32 = 48_000;
/// Largest frame (48 kHz / 100).
pub const MAX_FRAME: usize = 480;
/// Denoiser output capacity per frame, as the managed wrapper's scratch (frame × 8).
pub const DENOISER_OUTPUT_FRAMES: usize = 8;
pub const STARTUP_HOLD_FRAMES: u32 = 20;
pub const MAX_DENOISER_WAIT_FRAMES: u32 = 100;
pub const DENOISER_STALL_FRAMES: u32 = 100;
pub const TRANSMIT_FADE_MS: u32 = 5;
pub const MAX_VOICE_GAIN_DB: f32 = 18.0;
/// Auto sensitivity's open minimum (dBFS before the voice gain) while the host sets none.
pub const AUTO_SENSITIVITY_MINIMUM_DB: f32 = -54.0;
/// The range a host's minimum is clamped to.
pub const AUTO_SENSITIVITY_MINIMUM_RANGE_DB: (f32, f32) = (-80.0, -30.0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PushToTalk {
    #[default]
    VoiceActivity,
    Held,
    Released,
}

impl PushToTalk {
    /// 0 voice activity, 1 held, anything else released (closes the gate).
    pub fn from_wire(value: u32) -> Self {
        match value {
            0 => Self::VoiceActivity,
            1 => Self::Held,
            _ => Self::Released,
        }
    }
}

/// Read once per frame. Values need not be sanitised (the chain does: NaN volume/threshold → 0,
/// volume clamped 0..2, threshold 0..1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DspSettings {
    /// Rate of this frame; a supported frame has exactly rate/100 samples.
    pub sample_rate: u32,
    pub mic_muted: bool,
    pub push_to_talk: PushToTalk,
    /// Linear 0..2, after the noise gate (20 ms ramp).
    pub input_volume: f32,
    pub auto_gain: bool,
    /// Linear RMS of a ±1 signal, 0..1 (manual sensitivity).
    pub gate_threshold: f32,
    pub auto_sensitivity: bool,
    /// The host's NS mode is the Effects SDK: the startup hold waits for the denoiser (≤ 100 frames).
    pub denoiser_wanted: bool,
    /// Off, or what the speech detector's verdict qualifies (push-to-talk ignores it).
    pub speech_detection: SpeechDetection,
    /// The speech detector's probability for this frame's pre-denoise input, 0..1; negative or
    /// non-finite when it did not run (the level gate alone decides, as with Off).
    pub speech_probability: f32,
    /// The frame's raw capture (before the APM) was digital silence: no sample above -100 dBFS, so
    /// every int16 sample 0. Auto sensitivity learns nothing from it (a hardware mute).
    pub capture_silent: bool,
    /// Auto sensitivity's open minimum, dBFS before the voice gain: non-finite is
    /// AUTO_SENSITIVITY_MINIMUM_DB, the rest clamped to AUTO_SENSITIVITY_MINIMUM_RANGE_DB.
    pub auto_sensitivity_minimum_db: f32,
}

impl Default for DspSettings {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            mic_muted: false,
            push_to_talk: PushToTalk::VoiceActivity,
            input_volume: 1.0,
            auto_gain: true,
            gate_threshold: 0.025,
            auto_sensitivity: true,
            denoiser_wanted: false,
            speech_detection: SpeechDetection::Off,
            speech_probability: -1.0,
            capture_silent: false,
            auto_sensitivity_minimum_db: AUTO_SENSITIVITY_MINIMUM_DB,
        }
    }
}

/// One-shot commands for this frame, applied in this order before anything else.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DspCommands {
    /// Session start: every per-stream stage starts over and the voice gain restarts from 0 dB.
    pub reset: bool,
    /// Stream (re)start: aligner, stall count and startup hold start over.
    pub flush: bool,
    /// Restart the voice gain from gain_reset_db (non-finite: ignored; clamped 0..18).
    pub gain_reset: bool,
    pub gain_reset_db: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreDenoise {
    /// The chain was reset, flushed or reconfigured for a new rate this frame: flush the pipeline
    /// (flush(null, 0)) before process() if one runs this frame, else before its next run.
    pub flush_denoiser: bool,
    /// The frame is supported (rate in MIN_RATE..=max_rate and len == rate/100 > 0).
    pub supported: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum DenoiserOutput<'a> {
    /// No pipeline ran or will: none installed, bypassed, or not wanted. Frame passes as is; the
    /// aligner is reset (once) and the stall count cleared.
    NotInUse,
    /// A pipeline is installed but the slot was busy: the frame passes dry; aligner untouched.
    Missed,
    /// process() ran: its output (FloatS16), 0..=8×frame samples (extra samples are ignored).
    /// restart: first frame of a newly installed pipeline, or the pipeline was flushed before this
    /// call (the aligner is reset before the push).
    Ran { samples: &'a [f32], restart: bool },
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AlignerStats {
    pub primed: bool,
    pub buffered_samples: u32,
    /// Cumulative for the chain's lifetime.
    pub underruns: u64,
    pub padded_samples: u64,
    pub trimmed_samples: u64,
    pub drained_samples: u64,
    /// Latency the aligner added since its last reset (inserted minus drained and trimmed).
    pub latency_ms: f32,
    /// Managed DenoiserStats.LatencyMs: the same over the cumulative counters.
    pub lifetime_latency_ms: f32,
}

/// Written by every pre/post call (pre fills apm_out_sumsq, samples, unsupported, reset, flushed,
/// non_finite_samples; post fills the rest). Per-frame unless noted.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DspOutputs {
    /// Samples behind this frame's sums (the frame length).
    pub samples: u32,
    /// Sums of squares in FloatS16 (int16) units, channel 0: the sanitised input.
    pub apm_out_sumsq: f64,
    /// After the denoise stage (aligner output, or the input when not in use).
    pub denoised_sumsq: f64,
    /// After voice gain and startup hold (managed VoiceMicGateInput).
    pub gate_in_sumsq: f64,
    /// What leaves (after the transmit gate).
    pub sent_sumsq: f64,
    /// RMS (±1 units) after voice gain, before the hold: what auto sensitivity and the input duty
    /// see.
    pub gate_in_rms: f32,
    /// Loudest gate block (after the hold), dBFS of ±1; -inf when every block is silent.
    pub gate_in_block_peak_db: f32,
    /// The gate's threshold after this frame.
    pub threshold: f32,
    /// After this frame.
    pub envelope: f32,
    /// NoiseGate.IsSpeaking after this frame.
    pub gate_speaking: bool,
    /// gate_speaking && transmit_open.
    pub speaking: bool,
    /// envelope > 0.
    pub gate_open: bool,
    /// gate_in_rms > threshold.
    pub gate_input_above: bool,
    /// The startup hold zeroed this frame.
    pub held: bool,
    /// Transmit gate target (not muted, PTT not Released).
    pub transmit_open: bool,
    /// The transmit gate was above 0 for any sample of this frame.
    pub transmitting: bool,
    /// Last value while auto gain is off (0 before any and after a reset).
    pub voice_gain_db: f32,
    pub voice_gain_settled: bool,
    /// DenoiserOutput::Ran this frame.
    pub denoiser_ran: bool,
    /// ≥ DENOISER_STALL_FRAMES empty outputs in a row.
    pub denoiser_stalled: bool,
    pub aligner: AlignerStats,
    pub unsupported: bool,
    /// Replaced in this frame (input and denoiser output).
    pub non_finite_samples: u32,
    /// A reset ran this frame.
    pub reset: bool,
    /// A flush ran this frame (command, rate change or reset).
    pub flushed: bool,
    /// The probability the speech gate used this frame; -1 when it did not judge the frame.
    pub speech_probability: f32,
    /// Speech confirmed after this frame (speech detection on and judging).
    pub speech: bool,
    /// The speech gate kept the gate from opening or forced its release this frame.
    pub speech_closed: bool,
}
