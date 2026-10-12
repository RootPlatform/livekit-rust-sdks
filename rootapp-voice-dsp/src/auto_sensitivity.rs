use crate::clamp_f64;

const FRAME_MS: f64 = 10.0;

/// Managed `AutoSensitivityOptions`; Default = the C# defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoSensitivityOptions {
    pub initial_floor_db: f64,
    pub floor_minimum_db: f64,
    pub floor_maximum_db: f64,
    pub open_margin_db: f64,
    pub open_minimum_db: f64,
    pub hysteresis_db: f64,
    pub fall_time_constant_ms: f64,
    pub rise_db_per_second: f64,
    pub startup_rise_db_per_second: f64,
    pub startup_ms: f64,
    pub open_window_ms: f64,
}

impl Default for AutoSensitivityOptions {
    fn default() -> Self {
        Self {
            initial_floor_db: -60.0,
            floor_minimum_db: -90.0,
            floor_maximum_db: -35.0,
            open_margin_db: 8.0,
            open_minimum_db: -60.0,
            hysteresis_db: 3.0,
            fall_time_constant_ms: 1000.0,
            rise_db_per_second: 1.0,
            startup_rise_db_per_second: 10.0,
            startup_ms: 2000.0,
            open_window_ms: 2000.0,
        }
    }
}

/// Managed `AutoSensitivity`: the gate threshold follows the mic's noise floor, one `update` per
/// 10 ms frame. The floor and the thresholds are in dB of the denoised voice before the voice gain;
/// `update` adds the frame's gain to the threshold it returns.
#[derive(Clone, Debug)]
pub struct AutoSensitivity {
    options: AutoSensitivityOptions,
    recent_db: Vec<f64>,
    floor_db: f64,
    elapsed_ms: f64,
    recent_count: usize,
    recent_next: usize,
}

impl AutoSensitivity {
    pub fn new(options: AutoSensitivityOptions) -> Self {
        let window = ((options.open_window_ms / FRAME_MS) as usize).max(1);
        Self {
            floor_db: options.initial_floor_db,
            options,
            recent_db: vec![0.0; window],
            elapsed_ms: 0.0,
            recent_count: 0,
            recent_next: 0,
        }
    }

    pub fn options(&self) -> &AutoSensitivityOptions {
        &self.options
    }

    pub fn floor_db(&self) -> f64 {
        self.floor_db
    }

    pub fn open_db(&self) -> f64 {
        (self.floor_db + self.options.open_margin_db).max(self.options.open_minimum_db)
    }

    pub fn close_db(&self) -> f64 {
        self.open_db() - self.options.hysteresis_db
    }

    pub fn reset(&mut self) {
        self.floor_db = self.options.initial_floor_db;
        self.elapsed_ms = 0.0;
        self.recent_count = 0;
        self.recent_next = 0;
    }

    /// input_rms: the frame before the voice gain; gain_db: the voice gain applied to it (0 without
    /// one). Returns the RMS threshold the gate should use for the frame after the gain.
    pub fn update(&mut self, input_rms: f32, gain_db: f64, gate_open: bool) -> f32 {
        let level_db = to_db(input_rms);
        let len = self.recent_db.len();
        if let Some(slot) = self.recent_db.get_mut(self.recent_next) {
            *slot = level_db;
        }
        self.recent_next = self.recent_next.saturating_add(1).checked_rem(len).unwrap_or(0);
        self.recent_count = self.recent_count.saturating_add(1).min(len);

        let o = &self.options;
        let rise = (if self.elapsed_ms < o.startup_ms {
            o.startup_rise_db_per_second
        } else {
            o.rise_db_per_second
        }) * FRAME_MS
            / 1000.0;
        if !gate_open {
            if level_db < self.floor_db {
                self.floor_db += (level_db - self.floor_db)
                    * (1.0 - (-FRAME_MS / o.fall_time_constant_ms).exp());
            } else {
                self.floor_db += (level_db - self.floor_db).min(rise);
            }
        } else {
            let quietest_db = self.recent_minimum();
            if quietest_db > self.floor_db {
                self.floor_db += (quietest_db - self.floor_db).min(rise);
            }
        }
        self.floor_db =
            clamp_f64(self.floor_db, self.options.floor_minimum_db, self.options.floor_maximum_db);
        self.elapsed_ms += FRAME_MS;
        let gain_db = if gain_db.is_finite() { gain_db } else { 0.0 };
        to_rms(if gate_open { self.close_db() } else { self.open_db() } + gain_db)
    }

    fn recent_minimum(&self) -> f64 {
        let mut minimum = f64::MAX;
        for &db in self.recent_db.iter().take(self.recent_count) {
            minimum = minimum.min(db);
        }
        minimum
    }
}

pub fn to_db(rms: f32) -> f64 {
    20.0 * f64::from(rms.max(1e-7)).log10()
}

pub fn to_rms(db: f64) -> f32 {
    crate::pow10(db / 20.0) as f32
}
