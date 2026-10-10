pub const DEFAULT_RAMP_SAMPLES: u32 = 960;

/// Managed `GainRamp` (`VolumeCurve.cs`): linear toward the newest target over `ramp_samples`.
#[derive(Clone, Debug)]
pub struct GainRamp {
    ramp_samples: u32,
    step: f32,
    remaining: u32,
    current: f32,
    target: f32,
}

impl GainRamp {
    pub fn new(initial: f32, ramp_samples: u32) -> Self {
        Self {
            ramp_samples: ramp_samples.max(1),
            step: 0.0,
            remaining: 0,
            current: initial,
            target: initial,
        }
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn is_silent(&self) -> bool {
        self.current == 0.0 && self.target == 0.0
    }

    pub fn set_target(&mut self, target: f32) {
        if target == self.target {
            return;
        }
        self.target = target;
        self.remaining = self.ramp_samples;
        self.step = (target - self.current) / self.ramp_samples as f32;
    }

    pub fn apply(&mut self, samples: &mut [f32]) {
        let ramped = samples.len().min(self.remaining as usize);
        let Some((head, rest)) = samples.split_at_mut_checked(ramped) else {
            return;
        };
        for sample in head.iter_mut() {
            self.current += self.step;
            self.remaining = self.remaining.saturating_sub(1);
            if self.remaining == 0 {
                self.current = self.target;
            }
            *sample *= self.current;
        }
        if rest.is_empty() {
            return;
        }
        let gain = self.current;
        if gain == 1.0 {
            return;
        }
        if gain == 0.0 {
            rest.fill(0.0);
            return;
        }
        for sample in rest.iter_mut() {
            *sample *= gain;
        }
    }

    /// A new length, with the value at `value` and nothing in flight.
    pub fn snap(&mut self, value: f32, ramp_samples: u32) {
        *self = Self::new(value, ramp_samples);
    }
}
