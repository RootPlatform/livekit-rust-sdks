/// Mute and push-to-talk: linear toward open (1) or closed (0) over the fade, zero-fill while
/// closed (Phase 1 `RootCaptureProcessor::ApplyGate`).
#[derive(Clone, Debug)]
pub struct TransmitGate {
    gain: f32,
    step: f32,
}

impl TransmitGate {
    pub fn new(fade_samples: u32, open: bool) -> Self {
        Self { gain: if open { 1.0 } else { 0.0 }, step: 1.0 / fade_samples.max(1) as f32 }
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// A new fade length, at the target with no fade in flight.
    pub fn snap(&mut self, fade_samples: u32, open: bool) {
        *self = Self::new(fade_samples, open);
    }

    /// Returns whether any sample left with a gain above 0.
    pub fn apply(&mut self, samples: &mut [f32], open: bool) -> bool {
        let target = if open { 1.0f32 } else { 0.0 };
        if self.gain == target {
            if !open {
                samples.fill(0.0);
            }
            return open && !samples.is_empty();
        }
        let mut transmitting = false;
        for sample in samples.iter_mut() {
            self.gain = if self.gain < target {
                target.min(self.gain + self.step)
            } else {
                target.max(self.gain - self.step)
            };
            *sample *= self.gain;
            transmitting |= self.gain > 0.0;
        }
        transmitting
    }

    /// No fade (frames the chain cannot process): closed zero-fills, open leaves the samples, and
    /// the gate lands on the target so the next fade starts from what was sent.
    pub fn apply_hard(&mut self, samples: &mut [f32], open: bool) -> bool {
        self.gain = if open { 1.0 } else { 0.0 };
        if !open {
            samples.fill(0.0);
        }
        open && !samples.is_empty()
    }
}
