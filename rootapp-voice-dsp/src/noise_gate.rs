use crate::speech_gate::{GateControl, FAST_RELEASE_SECONDS};

pub const DEFAULT_THRESHOLD: f32 = 0.025;
pub const FULL_ON_ENVELOPE: f32 = 0.2;
pub const ATTACK_SECONDS: f64 = 0.02;
pub const RELEASE_SECONDS: f64 = 0.4;
pub const VAD_HOLD_SECONDS: f64 = 0.3;
/// The block at 48 kHz (managed `NoiseGate.BlockSamples`).
pub const BLOCK_SAMPLES: usize = 128;

/// The gate block at `rate`: 128 samples at 48 kHz, the same ≈2.67 ms at other rates.
pub fn block_samples_for(rate: u32) -> usize {
    let block = u64::from(rate).saturating_mul(128).saturating_add(24_000) / 48_000;
    usize::try_from(block).unwrap_or(usize::MAX).max(1)
}

/// Managed `NoiseGate` (`NoiseGate.cs`) with the block size a parameter: RMS per block above the
/// threshold opens a gain envelope (20 ms attack, 400 ms release, full gain at 0.2), speaking
/// follows the envelope plus a 300 ms hold; push-to-talk replaces the RMS with 1 while held and
/// closes at once when idle.
#[derive(Clone, Debug)]
pub struct NoiseGate {
    sample_rate: u32,
    block_samples: usize,
    hold_samples: i64,
    envelope: f32,
    /// The envelope the level alone would give (GateControl::LEVEL on every block); equal to
    /// `envelope` unless the speech gate intervened.
    level_envelope: f32,
    above_blocks: u32,
    position: i64,
    hold_end: i64,
    push_to_talk: Option<bool>,
    threshold: f32,
    min_open_blocks: u32,
    is_speaking: bool,
}

impl NoiseGate {
    pub fn new(sample_rate: u32, block_samples: usize) -> Self {
        let sample_rate = sample_rate.max(1);
        Self {
            sample_rate,
            block_samples: block_samples.max(1),
            hold_samples: (VAD_HOLD_SECONDS * f64::from(sample_rate)).round_ties_even() as i64,
            envelope: 0.0,
            level_envelope: 0.0,
            above_blocks: 0,
            position: 0,
            hold_end: 0,
            push_to_talk: None,
            threshold: DEFAULT_THRESHOLD,
            min_open_blocks: 0,
            is_speaking: false,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn block_samples(&self) -> usize {
        self.block_samples
    }

    pub fn hold_samples(&self) -> i64 {
        self.hold_samples
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold;
    }

    /// Blocks in a row above the threshold before a closed gate starts to open (0 = the first).
    pub fn min_open_blocks(&self) -> u32 {
        self.min_open_blocks
    }

    pub fn set_min_open_blocks(&mut self, blocks: u32) {
        self.min_open_blocks = blocks;
    }

    pub fn is_speaking(&self) -> bool {
        self.is_speaking
    }

    pub fn envelope(&self) -> f32 {
        self.envelope
    }

    /// The envelope without the speech gate's controls: what the level gate alone would have.
    /// Never below `envelope`.
    pub fn level_envelope(&self) -> f32 {
        self.level_envelope
    }

    pub fn push_to_talk(&self) -> Option<bool> {
        self.push_to_talk
    }

    pub fn set_push_to_talk(&mut self, push_to_talk: Option<bool>) {
        self.push_to_talk = push_to_talk;
        if push_to_talk == Some(false) {
            self.envelope = 0.0;
            self.level_envelope = 0.0;
            self.hold_end = 0;
            self.is_speaking = false;
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        self.process_gated(samples, true);
    }

    /// `apply` false: the state advances exactly as `process`, the samples are left alone.
    pub fn process_gated(&mut self, samples: &mut [f32], apply: bool) {
        self.process_controlled(samples, apply, GateControl::LEVEL);
    }

    /// `process_gated` with the speech gate's controls: `allow_open` false keeps a closed gate
    /// closed; `fast_close` releases over FAST_RELEASE_SECONDS whatever the level and skips the
    /// speaking hold; `eager_open` skips the min_open_blocks wait while `level_envelope` is open.
    /// The envelope never exceeds `level_envelope`. GateControl::LEVEL is exactly
    /// `process_gated`.
    pub fn process_controlled(&mut self, samples: &mut [f32], apply: bool, control: GateControl) {
        for block in samples.chunks_mut(self.block_samples) {
            let multiplier = self.advance(block, control);
            if apply {
                apply_multiplier(block, multiplier);
            }
        }
    }

    /// `process_controlled` with the level measured on `measure` and the gain applied to the same
    /// block of `apply` (a look-ahead: `apply` holds older audio). The state advances exactly as
    /// `process_controlled(measure, ..)` would; blocks past the shorter slice are not processed.
    pub fn process_split(
        &mut self,
        measure: &[f32],
        apply: &mut [f32],
        gain: bool,
        control: GateControl,
    ) {
        let block_samples = self.block_samples;
        for (measured, applied) in
            measure.chunks(block_samples).zip(apply.chunks_mut(block_samples))
        {
            let multiplier = self.advance(measured, control);
            if gain {
                apply_multiplier(applied, multiplier);
            }
        }
    }

    /// One block of the state on `block`'s level; returns the gain for the block.
    fn advance(&mut self, block: &[f32], control: GateControl) -> f32 {
        let block_seconds = block.len() as f64 / f64::from(self.sample_rate);
        let rms = match self.push_to_talk {
            None => compute_rms(block),
            Some(true) => 1.0,
            Some(false) => 0.0,
        };
        let above = rms > self.threshold;
        self.above_blocks = if above { self.above_blocks.saturating_add(1) } else { 0 };
        let level_open = self.level_envelope > 0.0;
        let level_attack = above && (level_open || self.above_blocks >= self.min_open_blocks);
        self.level_envelope = step(self.level_envelope, block_seconds, level_attack, above);
        if control.fast_close {
            self.envelope =
                (f64::from(self.envelope) - block_seconds / FAST_RELEASE_SECONDS).max(0.0) as f32;
            if self.envelope <= 0.0 {
                self.hold_end = self.position;
            }
        } else {
            let attack = above
                && (self.envelope > 0.0
                    || (control.allow_open
                        && (self.above_blocks >= self.min_open_blocks
                            || (control.eager_open && level_open))));
            self.envelope = step(self.envelope, block_seconds, attack, above);
        }

        let multiplier = (self.envelope / FULL_ON_ENVELOPE).min(1.0);

        if self.envelope > 0.0 {
            self.is_speaking = true;
            if !control.fast_close {
                self.hold_end = self.position.saturating_add(self.hold_samples);
            }
        } else if self.is_speaking && self.position >= self.hold_end {
            self.is_speaking = false;
        }

        self.position = self.position.saturating_add(block.len() as i64);
        multiplier
    }
}

fn apply_multiplier(block: &mut [f32], multiplier: f32) {
    if multiplier <= 0.0 {
        block.fill(0.0);
    } else if multiplier < 1.0 {
        for s in block.iter_mut() {
            *s *= multiplier;
        }
    }
}

/// One block of the envelope: up over ATTACK_SECONDS, down over RELEASE_SECONDS below the
/// threshold, else held.
fn step(envelope: f32, block_seconds: f64, attack: bool, above: bool) -> f32 {
    if attack {
        (f64::from(envelope) + block_seconds / ATTACK_SECONDS).min(1.0) as f32
    } else if !above {
        (f64::from(envelope) - block_seconds / RELEASE_SECONDS).max(0.0) as f32
    } else {
        envelope
    }
}

fn compute_rms(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let mut sum = 0.0f64;
    for &s in block {
        sum += f64::from(s) * f64::from(s);
    }
    (sum / block.len() as f64).sqrt() as f32
}
