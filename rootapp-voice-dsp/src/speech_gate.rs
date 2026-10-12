/// A frame is speech at this probability or above; CONFIRM_FRAMES in a row confirm speech (3: loud
/// synthetic clicks reach 2 frames above 0.5).
pub const SPEECH_ON: f32 = 0.5;
/// Confirmed speech continues while frames stay at this probability or above.
pub const SPEECH_OFF: f32 = 0.3;
pub const CONFIRM_FRAMES: u32 = 3;
/// Frames below SPEECH_OFF before confirmed speech ends (300 ms).
pub const HANG_FRAMES: u32 = 30;
/// A level open the detector has not confirmed lasts at most this many frames (200 ms).
pub const PROVISIONAL_FRAMES: u32 = 20;
/// Frames below the threshold in a row that let the level open the gate again by itself after
/// the detector closed it (100 ms).
pub const REARM_FRAMES: u32 = 10;
/// Release of a close the detector forced.
pub const FAST_RELEASE_SECONDS: f64 = 0.05;

/// What the detector's verdict qualifies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpeechDetection {
    /// Level gate only (the managed engine's behaviour).
    #[default]
    Off,
    /// The ring and the speaking byte need confirmed speech; the audio follows the level gate.
    Ring,
    /// The ring as Ring; the audio gate also closes on what the detector does not confirm.
    Gate,
}

impl SpeechDetection {
    /// 1 ring, 2 gate, anything else off.
    pub fn from_wire(value: u32) -> Self {
        match value {
            1 => Self::Ring,
            2 => Self::Gate,
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
}

impl GateControl {
    pub const LEVEL: Self = Self { allow_open: true, fast_close: false };
}

/// Confirms the level gate's opens with the speech detector's probability, one frame (10 ms) at a
/// time: `observe` before AutoSensitivity, `control` once the threshold is known, `after_gate` once
/// the gate ran.
#[derive(Clone, Debug, Default)]
pub struct SpeechGate {
    speech_run: u32,
    confirmed: bool,
    since_speech: u32,
    provisional: u32,
    detector_required: bool,
    quiet: u32,
    closing: bool,
}

impl SpeechGate {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Speech confirmed: the ring may be on.
    pub fn confirmed(&self) -> bool {
        self.confirmed
    }

    /// The detector closed the gate and holds it shut until it confirms speech or the level drops.
    pub fn detector_required(&self) -> bool {
        self.detector_required
    }

    /// Advances the verdict with this frame's probability. `level_open`: the gate's envelope
    /// before this frame; `gate_audio`: SpeechDetection::Gate. Returns what AutoSensitivity
    /// should take as the gate's state: speech (confirmed, or an open not yet judged) does not
    /// teach the floor, anything the detector rejected does.
    pub fn observe(&mut self, p: f32, level_open: bool, gate_audio: bool) -> bool {
        self.speech_run = if p >= SPEECH_ON { self.speech_run.saturating_add(1) } else { 0 };
        if self.confirmed {
            self.since_speech =
                if p >= SPEECH_OFF { 0 } else { self.since_speech.saturating_add(1) };
            if self.since_speech > HANG_FRAMES {
                self.confirmed = false;
                if gate_audio && level_open {
                    self.closing = true;
                    self.detector_required = true;
                }
            }
        }
        if !self.confirmed && self.speech_run >= CONFIRM_FRAMES {
            self.confirmed = true;
            self.since_speech = 0;
            self.detector_required = false;
            self.closing = false;
        }
        if gate_audio {
            self.confirmed
                || (level_open && !self.detector_required && self.provisional <= PROVISIONAL_FRAMES)
        } else {
            level_open
        }
    }

    /// SpeechDetection::Gate only. `above`: this frame's gate input is above the threshold.
    pub fn control(&mut self, p: f32, above: bool, level_open: bool) -> GateControl {
        self.quiet = if above { 0 } else { self.quiet.saturating_add(1) };
        if self.detector_required && self.quiet >= REARM_FRAMES {
            self.detector_required = false;
        }
        if level_open && !self.confirmed {
            self.provisional = self.provisional.saturating_add(1);
            if self.provisional > PROVISIONAL_FRAMES {
                self.closing = true;
                self.detector_required = true;
            }
        } else {
            self.provisional = 0;
        }
        GateControl {
            allow_open: !self.detector_required || p >= SPEECH_ON,
            fast_close: self.closing && !self.confirmed,
        }
    }

    /// SpeechDetection::Gate only, with the envelope after the gate ran.
    pub fn after_gate(&mut self, envelope: f32) {
        if envelope <= 0.0 {
            self.closing = false;
            self.provisional = 0;
        }
    }
}
