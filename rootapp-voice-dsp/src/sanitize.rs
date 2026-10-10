/// FloatS16 samples are clamped to ±4.0 of a ±1 signal: the APM never legitimately exceeds it and
/// `s * s` stays finite in f32.
pub const SAMPLE_LIMIT: f32 = 131_072.0;
/// Smaller magnitudes become 0 (denormal guard: squares of ±1 values below about 3.6e-15 FloatS16
/// are subnormal in f32).
pub const DENORMAL_LIMIT: f32 = 1e-12;
/// Full scale of a FloatS16 signal.
pub const FULL_SCALE: f32 = 32_768.0;
pub const MAX_INPUT_VOLUME: f32 = 2.0;

/// Non-finite → 0 (counted), denormal-sized → 0, clamp to ±SAMPLE_LIMIT. Returns the number of
/// non-finite samples replaced.
pub fn sanitize(samples: &mut [f32]) -> u32 {
    let mut replaced = 0u32;
    for sample in samples.iter_mut() {
        let value = *sample;
        *sample = if !value.is_finite() {
            replaced = replaced.saturating_add(1);
            0.0
        } else if value.abs() < DENORMAL_LIMIT {
            0.0
        } else {
            value.clamp(-SAMPLE_LIMIT, SAMPLE_LIMIT)
        };
    }
    replaced
}

/// Last line before a frame leaves: non-finite → 0, clamp to full scale.
pub fn clamp_full_scale(samples: &mut [f32]) {
    for sample in samples.iter_mut() {
        let value = *sample;
        *sample = if value.is_finite() { value.clamp(-FULL_SCALE, FULL_SCALE) } else { 0.0 };
    }
}

/// NaN → 0, clamped 0..2.
pub fn input_volume(volume: f32) -> f32 {
    if volume.is_nan() {
        0.0
    } else {
        volume.clamp(0.0, MAX_INPUT_VOLUME)
    }
}

/// NaN → 0, clamped 0..1.
pub fn gate_threshold(threshold: f32) -> f32 {
    if threshold.is_nan() {
        0.0
    } else {
        threshold.clamp(0.0, 1.0)
    }
}

pub fn sum_squares(samples: &[f32]) -> f64 {
    let mut sum = 0.0f64;
    for &sample in samples {
        sum += f64::from(sample) * f64::from(sample);
    }
    sum
}
