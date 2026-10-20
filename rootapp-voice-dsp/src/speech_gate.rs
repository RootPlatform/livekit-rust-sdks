/// A frame is speech at this probability or above; CONFIRM_FRAMES in a row confirm speech (3: loud
/// synthetic clicks reach 2 frames above 0.5).
pub const SPEECH_ON: f32 = 0.5;
/// Confirmed speech continues while frames stay at this probability or above.
pub const SPEECH_OFF: f32 = 0.3;
pub const CONFIRM_FRAMES: u32 = 3;
/// Frames below SPEECH_OFF before confirmed speech ends (300 ms).
pub const HANG_FRAMES: u32 = 30;
/// At or below this RMS (-66 dBFS, under every threshold the sensitivity slider or auto
/// sensitivity sets) a frame is quiet whatever the threshold and opens nothing.
pub const QUIET_RMS: f32 = 0.0005;
/// Release of a close the detector forced.
pub const FAST_RELEASE_SECONDS: f64 = 0.05;
/// Frames in a row at SPEECH_OFF or more before the speech-loss counters judge a frame (100 ms,
/// longer than the denoiser's latency at any rate).
pub const STEADY_SPEECH_FRAMES: u32 = 10;

/// What the detector's verdict qualifies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpeechDetection {
    /// Level gate only (the managed engine's behaviour).
    #[default]
    Off,
    /// Only confirmed speech goes out.
    Gate,
}

impl SpeechDetection {
    /// 2 gate (and 1, the removed ring mode), anything else off.
    pub fn from_wire(value: u32) -> Self {
        match value {
            1 | 2 => Self::Gate,
            _ => Self::Off,
        }
    }
}

/// Non-finite or negative: no probability this frame (the detector did not run); else clamped to 1.
pub fn probability(value: f32) -> Option<f32> {
    if value.is_finite() && value >= 0.0 {
        Some(value.min(1.0))
    } else {
        None
    }
}

/// This frame's controls for the noise gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateControl {
    /// A closed gate may start to open.
    pub allow_open: bool,
    /// Release over FAST_RELEASE_SECONDS whatever the level, without the speaking hold.
    pub fast_close: bool,
    /// The gate opens on its first block above the threshold while the level gate alone is open,
    /// as that one would attack (confirmed speech the gate was not allowed to open for).
    pub eager_open: bool,
}

impl GateControl {
    pub const LEVEL: Self = Self { allow_open: true, fast_close: false, eager_open: false };
}

/// The speech detector's verdict, one frame (10 ms) at a time: `observe` before AutoSensitivity,
/// `control` once the threshold is known, `after_gate` once the gate ran.
///
/// Gate mode sends only confirmed speech: the level gate may open only while speech is confirmed,
/// so what the detector does not confirm (keys, clicks, a trackpad, breath, noise, the first frames
/// of a word before the confirmation) never goes out. When confirmed speech ends with the gate
/// still open, it closes over FAST_RELEASE_SECONDS; it opens again only on newly confirmed speech.
/// A verdict that starts over (a flush, a mode change) closes nothing: an open gate keeps its
/// level gate's release. The chain applies each frame's decision to the audio
/// DspSettings::speech_lookahead_ms older, so the frames of a word before its confirmation go out
/// with it when the confirmation comes within that time.
#[derive(Clone, Debug, Default)]
pub struct SpeechGate {
    speech_run: u32,
    confirmed: bool,
    since_speech: u32,
    ended: bool,
    closing: bool,
}

impl SpeechGate {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Speech confirmed: the gate may open.
    pub fn confirmed(&self) -> bool {
        self.confirmed
    }

    /// Advances the verdict with this frame's probability.
    pub fn observe(&mut self, p: f32) {
        self.speech_run = if p >= SPEECH_ON { self.speech_run.saturating_add(1) } else { 0 };
        if self.confirmed {
            self.since_speech =
                if p >= SPEECH_OFF { 0 } else { self.since_speech.saturating_add(1) };
            if self.since_speech > HANG_FRAMES {
                self.confirmed = false;
                self.ended = true;
            }
        }
        if !self.confirmed && self.speech_run >= CONFIRM_FRAMES {
            self.confirmed = true;
            self.since_speech = 0;
            self.ended = false;
            self.closing = false;
        }
    }

    /// SpeechDetection::Gate only. `rms`: this frame's gate input; `level_open`: the gate's
    /// envelope before this frame.
    pub fn control(&mut self, rms: f32, level_open: bool) -> GateControl {
        if std::mem::take(&mut self.ended) && level_open {
            self.closing = true;
        }
        GateControl {
            allow_open: rms > QUIET_RMS && self.confirmed,
            fast_close: self.closing && !self.confirmed,
            eager_open: self.confirmed,
        }
    }

    /// SpeechDetection::Gate only, with the envelope after the gate ran.
    pub fn after_gate(&mut self, envelope: f32) {
        if envelope <= 0.0 {
            self.closing = false;
        }
    }
}
