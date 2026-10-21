#![forbid(unsafe_code)]

pub mod agc2;
pub mod aligner;
pub mod auto_sensitivity;
mod chain;
pub mod frame_delay;
pub mod gain_ramp;
pub mod noise_gate;
pub mod sanitize;
pub mod speech_gate;
pub mod transmit_gate;
pub mod voice_gain;

pub use agc2::Agc2Tuning;
pub use chain::{DspChain, AUTO_MIN_OPEN_BLOCKS};
pub use speech_gate::SpeechDetection;
pub use voice_gain::{GainLimit, VoiceGainTuning};

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
/// Gain resets are clamped to 0..=this for the chain's own voice gain, and so is the maximum a host
/// can set for it (the default maximum is VoiceGainTuning::DEFAULT, 24 dB).
pub const MAX_VOICE_GAIN_DB: f32 = 24.0;
/// Auto sensitivity's open minimum (dBFS before the voice gain) while the host sets none.
pub const AUTO_SENSITIVITY_MINIMUM_DB: f32 = -54.0;
/// The range a host's minimum is clamped to.
pub const AUTO_SENSITIVITY_MINIMUM_RANGE_DB: (f32, f32) = (-80.0, -30.0);
/// The speech gate's look-ahead while the host sets none, ms.
pub const SPEECH_LOOKAHEAD_MS: f32 = 40.0;
/// The range a host's look-ahead is clamped to, ms (then rounded to whole 10 ms frames).
pub const SPEECH_LOOKAHEAD_RANGE_MS: (f32, f32) = (0.0, 60.0);
/// Frames the look-ahead's delay line holds at most (SPEECH_LOOKAHEAD_RANGE_MS's top).
pub const MAX_LOOKAHEAD_FRAMES: usize = 6;

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

/// Which voice gain boosts the denoised voice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GainEngine {
    /// The chain's own (`voice_gain::VoiceGain`, the managed engine's twin).
    #[default]
    Legacy,
    /// The host's (libwebrtc's AGC2), between `post_denoise_align` and `post_gain`, or ahead of the
    /// denoiser through `pre_denoise_gain`; `post_gain` gets the gain it applied. The chain's voice
    /// gain still measures the room and the talker's level for the stats, and its speech heard says
    /// when the gain is settled.
    External,
}

impl GainEngine {
    /// 1 external, anything else legacy.
    pub fn from_wire(value: u32) -> Self {
        match value {
            1 => Self::External,
            _ => Self::Legacy,
        }
    }

    pub fn to_wire(self) -> u32 {
        match self {
            Self::Legacy => 0,
            Self::External => 1,
        }
    }
}

/// What sets the noise gate's open threshold (DspOutputs::threshold_term).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThresholdTerm {
    /// No frame judged yet.
    #[default]
    None,
    /// Auto sensitivity: the noise floor plus its margin.
    Floor,
    /// Auto sensitivity: its minimum (lowered by the gain above its cap).
    Minimum,
    /// The manual threshold.
    Manual,
}

impl ThresholdTerm {
    pub fn to_wire(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Floor => 1,
            Self::Minimum => 2,
            Self::Manual => 3,
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
    /// non-finite when it did not run (the level gate alone decides, as with Off). The voice gain
    /// uses it in every mode whenever it is valid: only frames at its minimum probability or above
    /// are the person's speech.
    pub speech_probability: f32,
    /// The frame's raw capture (before the APM) was digital silence: no sample above -100 dBFS, so
    /// every int16 sample 0. Auto sensitivity learns nothing from it (a hardware mute).
    pub capture_silent: bool,
    /// Auto sensitivity's open minimum, dBFS before the voice gain: non-finite is
    /// AUTO_SENSITIVITY_MINIMUM_DB, the rest clamped to AUTO_SENSITIVITY_MINIMUM_RANGE_DB.
    pub auto_sensitivity_minimum_db: f32,
    /// The voice gain's tunables as the host set them: non-finite fields are the defaults, the rest
    /// clamped (VoiceGainTuning::sanitized).
    pub voice_gain: VoiceGainTuning,
    /// Which voice gain boosts the voice on this frame.
    pub gain_engine: GainEngine,
    /// The AGC2 the host ran on this frame (External): which limit holds its gain.
    pub agc2: Agc2Tuning,
    /// The speech gate's look-ahead (Gate mode with a probability, no push-to-talk), ms:
    /// non-finite is SPEECH_LOOKAHEAD_MS, the rest clamped to SPEECH_LOOKAHEAD_RANGE_MS and
    /// rounded to whole frames. The gate decides on each frame and applies that to the audio this
    /// many frames older, so the start of a word the detector confirms within it goes out.
    pub speech_lookahead_ms: f32,
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
            voice_gain: VoiceGainTuning::UNSET,
            gain_engine: GainEngine::Legacy,
            agc2: Agc2Tuning::UNSET,
            speech_lookahead_ms: f32::NAN,
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
    /// Restart the voice gain from gain_reset_db (non-finite: ignored; clamped 0..18, then to the voice gain's maximum).
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

/// What `post_denoise_align` left in the frame: the aligned voice before any gain (FloatS16) for the
/// host's gain, or with the gain `pre_denoise_gain` was given, and its RMS before any gain (±1
/// units) for `post_gain`. An unsupported frame is left as it came.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Aligned {
    pub supported: bool,
    pub pre_gain_rms: f32,
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
    /// The host did not hand the frame to the pipeline (its raw capture was digital silence): the
    /// frame becomes silence; the aligner, the stall count and the startup hold's wait for the
    /// denoiser are untouched, so the denoiser's stream carries on where it stopped.
    Skipped,
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
    /// After voice gain and startup hold (managed VoiceMicGateInput): the audio the gate's gain
    /// applies to, which the look-ahead delays like the frame sent.
    pub gate_in_sumsq: f64,
    /// What leaves (after the transmit gate).
    pub sent_sumsq: f64,
    /// RMS (±1 units) after voice gain, before the hold, of this frame (never delayed): what auto
    /// sensitivity, the gate's level and the input duty see.
    pub gate_in_rms: f32,
    /// Loudest block of gate_in (after the hold), dBFS of ±1; -inf when every block is silent.
    pub gate_in_block_peak_db: f32,
    /// The gate's threshold after this frame.
    pub threshold: f32,
    /// After this frame.
    pub envelope: f32,
    /// NoiseGate.IsSpeaking after this frame.
    pub gate_speaking: bool,
    /// Audio goes out: gate_speaking (the gate passes audio, or its hold after it did) or the frame
    /// sent carries audio, while the transmit gate is open, outside the startup hold (unsupported
    /// frames: whatever is sent). The ring and the speaking byte follow it, so what is sent rings
    /// (but the fade of a mute).
    pub speaking: bool,
    /// envelope > 0.
    pub gate_open: bool,
    /// gate_in_rms > threshold.
    pub gate_input_above: bool,
    /// The startup hold zeroed the frame sent (with the look-ahead: when it went in; the delay
    /// line's first frames after a stream start count as held).
    pub held: bool,
    /// Transmit gate target (not muted, PTT not Released) for the frame sent: with the look-ahead
    /// it was open both now and when the frame went in, so nothing captured while muted leaves.
    pub transmit_open: bool,
    /// The transmit gate was above 0 for any sample of this frame.
    pub transmitting: bool,
    /// Last value while auto gain is off (0 before any and after a reset). External: the host's.
    pub voice_gain_db: f32,
    pub voice_gain_settled: bool,
    /// The engine that boosted this frame.
    pub gain_engine: GainEngine,
    /// The chain's voice gain's view, whichever engine boosts (last values while auto gain is off):
    /// the room before the denoiser in the talker's pauses and the talker's level before the gain
    /// (NaN until measured), the floor after the denoiser before the gain, and what held the gain.
    pub voice_gain_room_db: f32,
    pub voice_gain_speech_level_db: f32,
    pub voice_gain_floor_db: f32,
    pub voice_gain_limit: GainLimit,
    /// DenoiserOutput::Ran this frame.
    pub denoiser_ran: bool,
    /// DenoiserOutput::Skipped this frame.
    pub denoiser_skipped: bool,
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
    /// The look-ahead applied to this frame, ms (0: none; the latency it adds).
    pub speech_lookahead_ms: u32,
    /// Steady confirmed speech (speech_gate::STEADY_SPEECH_FRAMES) the denoiser erased: its aligned
    /// output is 15 dB or more under every pre-denoise frame of the last STEADY_SPEECH_FRAMES
    /// (which hold the frame it came from), transmit open, outside the startup hold.
    pub speech_denoiser_erased: bool,
    /// The frame sent was steady confirmed speech above QUIET_RMS when it went in, not erased by
    /// the denoiser, and the level gate was shut for all of it (under the threshold), transmit
    /// open, push-to-talk not in use.
    pub speech_gate_cut: bool,
    /// Auto sensitivity's noise floor, dB before the gain (NaN while it is off).
    pub auto_sensitivity_floor_db: f32,
    /// The gate's open threshold in dB before the gain this frame (auto sensitivity's, or the
    /// manual threshold less the gain; at least -140).
    pub open_threshold_db: f32,
    /// What set it.
    pub threshold_term: ThresholdTerm,
}
