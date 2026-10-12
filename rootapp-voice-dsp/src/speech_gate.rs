/// A frame is speech at this probability or above; CONFIRM_FRAMES in a row confirm speech (3: loud
/// synthetic clicks reach 2 frames above 0.5).
pub const SPEECH_ON: f32 = 0.5;
/// Confirmed speech continues while frames stay at this probability or above. Such a frame also
/// opens a gate the detector holds shut and does not use up the provisional window.
pub const SPEECH_OFF: f32 = 0.3;
pub const CONFIRM_FRAMES: u32 = 3;
/// Frames below SPEECH_OFF before confirmed speech ends (300 ms).
pub const HANG_FRAMES: u32 = 30;
/// A level open the detector has not confirmed lasts at most this many frames below SPEECH_OFF
/// (200 ms).
pub const PROVISIONAL_FRAMES: u32 = 20;
/// Quiet frames in a row that let the level open the gate again by itself after the detector
/// closed it (100 ms).
pub const REARM_FRAMES: u32 = 10;
/// Quiet frames before a frame above the threshold that make it a new onset (30 ms), if it is also
/// REARM_RISE louder than the last of them (input flickering around the threshold makes none):
/// when the hang runs out on one it gets its own provisional window, and it ends the release of a
/// tail.
pub const ONSET_QUIET_FRAMES: u32 = 3;
/// An onset that stays above the threshold this many frames opens a gate the detector holds.
pub const ONSET_RUN_FRAMES: u32 = 3;
/// A frame louder than what the detector rejected by this RMS ratio (6 dB) lets the level open
/// the gate again.
pub const REARM_RISE: f32 = 2.0;
/// At or below this RMS (-66 dBFS, under every threshold the sensitivity slider or auto
/// sensitivity sets) a frame is quiet whatever the threshold and does not start an open, so a
/// threshold of 0 still re-arms.
pub const QUIET_RMS: f32 = 0.0005;
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
    /// A new onset: a gate the speech gate closed opens on its first block above the threshold
    /// while the level gate alone would still be open, as that one would attack.
    pub eager_open: bool,
}

impl GateControl {
    pub const LEVEL: Self = Self { allow_open: true, fast_close: false, eager_open: false };
}

/// Confirms the level gate's opens with the speech detector's probability, one frame (10 ms) at a
/// time: `observe` before AutoSensitivity, `control` once the threshold is known, `after_gate` once
/// the gate ran.
///
/// Gate mode closes an open the detector has not confirmed within the provisional window, and the
/// gate when confirmed speech ends with the level still open: at once (holding it shut) when the
/// input is held above the threshold, as a fast release of the tail when it is already quiet. A
/// held gate opens again on a frame at SPEECH_OFF, after REARM_FRAMES quiet frames, on a frame
/// REARM_RISE louder than what was rejected, or on an onset that lasts ONSET_RUN_FRAMES.
#[derive(Clone, Debug, Default)]
pub struct SpeechGate {
    speech_run: u32,
    confirmed: bool,
    since_speech: u32,
    provisional: u32,
    detector_required: bool,
    quiet: u32,
    /// The input of the last quiet frame.
    last_quiet: f32,
    closing: bool,
    hang_ended: bool,
    /// Frames of the current run of non-quiet frames since an onset or a rise in it (else 0).
    onset_run: u32,
    /// What the detector rejected: the loudest input since the last frame at SPEECH_OFF or above
    /// (any frame while it holds the gate). The last two frames join it two frames late, so a
    /// rising onset is measured against what came before it.
    peak: f32,
    recent: [f32; 2],
}

impl SpeechGate {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Speech confirmed: the ring may be on.
    pub fn confirmed(&self) -> bool {
        self.confirmed
    }

    /// The detector closed the gate and holds it shut until speech, quiet or a louder onset.
    pub fn detector_required(&self) -> bool {
        self.detector_required
    }

    /// Advances the verdict with this frame's probability. `level_open`: the gate's envelope
    /// before this frame; `gate_audio`: SpeechDetection::Gate.
    pub fn observe(&mut self, p: f32, level_open: bool, gate_audio: bool) {
        self.speech_run = if p >= SPEECH_ON { self.speech_run.saturating_add(1) } else { 0 };
        if self.confirmed {
            self.since_speech =
                if p >= SPEECH_OFF { 0 } else { self.since_speech.saturating_add(1) };
            if self.since_speech > HANG_FRAMES {
                self.confirmed = false;
                self.hang_ended = gate_audio && level_open;
            }
        }
        if !self.confirmed && self.speech_run >= CONFIRM_FRAMES {
            self.confirmed = true;
            self.since_speech = 0;
            self.release();
        }
    }

    /// SpeechDetection::Gate only. `rms` and `threshold`: this frame's gate input and the gate's
    /// threshold for it.
    pub fn control(&mut self, p: f32, rms: f32, threshold: f32, level_open: bool) -> GateControl {
        let quiet = rms <= threshold || rms <= QUIET_RMS;
        let onset = !quiet
            && self.quiet >= ONSET_QUIET_FRAMES
            && rms > self.last_quiet * REARM_RISE;
        if quiet {
            self.last_quiet = rms;
        }
        self.quiet = if quiet { self.quiet.saturating_add(1) } else { 0 };
        let rise = self.peak > 0.0 && rms > self.peak * REARM_RISE;
        self.onset_run = if quiet {
            0
        } else if self.onset_run > 0 {
            self.onset_run.saturating_add(1)
        } else if onset || rise {
            1
        } else {
            0
        };
        if p >= SPEECH_OFF && !self.detector_required {
            self.peak = 0.0;
            self.recent = [0.0; 2];
        } else {
            let [older, newer] = self.recent;
            self.peak = self.peak.max(older);
            self.recent = [newer, rms];
        }

        if std::mem::take(&mut self.hang_ended) {
            if quiet {
                self.closing = true;
            } else if self.onset_run > 0 {
                self.provisional = self.onset_run.saturating_sub(1);
            } else {
                self.hold();
            }
        } else if self.detector_required {
            if rise || self.onset_run >= ONSET_RUN_FRAMES {
                self.release();
            } else if self.quiet >= REARM_FRAMES {
                self.detector_required = false;
            }
        } else if self.closing && (onset || rise) {
            self.release();
        } else if rise {
            self.provisional = 0;
        }

        if level_open && !self.confirmed {
            if p < SPEECH_OFF {
                self.provisional = self.provisional.saturating_add(1);
            }
            if self.provisional > PROVISIONAL_FRAMES && !self.closing {
                self.hold();
            }
        } else {
            self.provisional = 0;
        }
        GateControl {
            allow_open: rms > QUIET_RMS && (!self.detector_required || p >= SPEECH_OFF),
            fast_close: self.closing && !self.confirmed,
            eager_open: self.onset_run > 0 || rise,
        }
    }

    /// SpeechDetection::Gate only, with the envelope after the gate ran.
    pub fn after_gate(&mut self, envelope: f32) {
        if envelope <= 0.0 {
            self.closing = false;
            self.provisional = 0;
        } else if self.detector_required && !self.closing {
            self.release();
        }
    }

    /// Close now and hold the gate shut; only an onset after this one can open it again.
    fn hold(&mut self) {
        self.closing = true;
        self.detector_required = true;
        self.onset_run = 0;
    }

    /// A new open: no close, no hold, a fresh provisional window.
    fn release(&mut self) {
        self.closing = false;
        self.detector_required = false;
        self.provisional = 0;
    }
}
