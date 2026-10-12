use crate::{clamp_f64, pow10};

const FRAME_MS: f64 = 10.0;

/// Managed `VoiceGainOptions` (`VoiceGain.cs`); Default = the C# defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceGainOptions {
    pub target_db: f64,
    pub max_gain_db: f64,
    pub max_output_noise_db: f64,
    pub max_room_noise_db: f64,
    pub foreground_margin_db: f64,
    pub room_noise_time_constant_ms: f64,
    pub room_guard_ms: f64,
    pub room_limit_fall_db_per_second: f64,
    pub speech_margin_db: f64,
    pub speech_minimum_db: f64,
    pub level_time_constant_ms: f64,
    pub min_speech_ms: f64,
    pub settled_speech_ms: f64,
    pub rise_db_per_second: f64,
    pub fall_db_per_second: f64,
    pub unsettled_db_per_second: f64,
    pub floor_fall_time_constant_ms: f64,
    pub floor_rise_db_per_second: f64,
    pub speech_run_frames: u32,
    pub noise_time_constant_ms: f64,
    pub noise_settle_ms: f64,
    pub noise_after_speech_ms: f64,
    pub limiter_ceiling: f32,
}

impl Default for VoiceGainOptions {
    fn default() -> Self {
        Self {
            target_db: -22.0,
            max_gain_db: 18.0,
            max_output_noise_db: -60.0,
            max_room_noise_db: -50.0,
            foreground_margin_db: 12.0,
            room_noise_time_constant_ms: 2000.0,
            room_guard_ms: 200.0,
            room_limit_fall_db_per_second: 10.0,
            speech_margin_db: 10.0,
            speech_minimum_db: -62.0,
            level_time_constant_ms: 1500.0,
            min_speech_ms: 500.0,
            settled_speech_ms: 3000.0,
            rise_db_per_second: 3.0,
            fall_db_per_second: 6.0,
            unsettled_db_per_second: 10.0,
            floor_fall_time_constant_ms: 300.0,
            floor_rise_db_per_second: 2.0,
            speech_run_frames: 3,
            noise_time_constant_ms: 2000.0,
            noise_settle_ms: 1000.0,
            noise_after_speech_ms: 250.0,
            limiter_ceiling: 0.9,
        }
    }
}

/// Managed `VoiceGain`: automatic gain for the denoised voice, one `process` per 10 ms frame.
#[derive(Clone, Debug)]
pub struct VoiceGain {
    options: VoiceGainOptions,
    foreground_ratio: f64,
    pending_room: Vec<f64>,
    gain_db: f64,
    floor_db: f64,
    speech_power: f64,
    speech_ms: f64,
    floor_known: bool,
    noise_power: f64,
    run: u32,
    run_power: f64,
    elapsed_ms: f64,
    since_speech_ms: f64,
    room_power: f64,
    input_power: f64,
    pending_next: usize,
}

impl VoiceGain {
    pub fn new(start_gain_db: f64, options: VoiceGainOptions) -> Self {
        let foreground_ratio = pow10(-options.foreground_margin_db / 10.0);
        let pending = ((options.room_guard_ms / FRAME_MS) as usize).max(1);
        let mut gain = Self {
            options,
            foreground_ratio,
            pending_room: vec![-1.0; pending],
            gain_db: 0.0,
            floor_db: -90.0,
            speech_power: -1.0,
            speech_ms: 0.0,
            floor_known: false,
            noise_power: -1.0,
            run: 0,
            run_power: 0.0,
            elapsed_ms: 0.0,
            since_speech_ms: f64::MAX,
            room_power: -1.0,
            input_power: -1.0,
            pending_next: 0,
        };
        gain.reset(start_gain_db);
        gain
    }

    pub fn options(&self) -> &VoiceGainOptions {
        &self.options
    }

    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    pub fn speech_level_db(&self) -> f64 {
        if self.speech_power < 0.0 {
            f64::NEG_INFINITY
        } else {
            10.0 * self.speech_power.max(1e-12).log10()
        }
    }

    pub fn floor_db(&self) -> f64 {
        self.floor_db
    }

    /// Average level of everything that is not speech, short bursts (key clicks) included.
    pub fn noise_db(&self) -> f64 {
        if self.noise_power < 0.0 {
            -90.0
        } else {
            10.0 * self.noise_power.max(1e-12).log10()
        }
    }

    pub fn speech_ms(&self) -> f64 {
        self.speech_ms
    }

    /// Average level of the room before the denoiser in the person's pauses (NaN until measured).
    pub fn room_noise_db(&self) -> f64 {
        if self.room_power < 0.0 {
            f64::NAN
        } else {
            10.0 * self.room_power.max(1e-12).log10()
        }
    }

    /// Enough speech heard that the gain is worth remembering for this microphone.
    pub fn is_settled(&self) -> bool {
        self.speech_ms >= self.options.settled_speech_ms
    }

    pub fn reset(&mut self, start_gain_db: f64) {
        let start = if start_gain_db.is_nan() { 0.0 } else { start_gain_db };
        self.gain_db = clamp_f64(start, 0.0, self.options.max_gain_db);
        self.floor_db = -90.0;
        self.floor_known = false;
        self.speech_power = -1.0;
        self.speech_ms = 0.0;
        self.noise_power = -1.0;
        self.run = 0;
        self.run_power = 0.0;
        self.elapsed_ms = 0.0;
        self.since_speech_ms = f64::MAX;
        self.room_power = -1.0;
        self.input_power = -1.0;
        self.pending_room.fill(-1.0);
        self.pending_next = 0;
    }

    /// The frame as it was before the denoiser; call it before `process` for the same frame.
    pub fn observe_input(&mut self, frame: &[f32]) {
        if frame.is_empty() {
            return;
        }
        let mut power = 0.0f64;
        for &s in frame {
            power += f64::from(s * s);
        }
        self.input_power = power / frame.len() as f64;
    }

    pub fn process(&mut self, frame: &mut [f32]) {
        if frame.is_empty() {
            return;
        }
        let mut power = 0.0f64;
        let mut peak = 0.0f32;
        for &s in frame.iter() {
            power += f64::from(s * s);
            peak = peak.max(s.abs());
        }
        power /= frame.len() as f64;
        let level_db = 10.0 * power.max(1e-12).log10();

        self.elapsed_ms += FRAME_MS;
        if self.since_speech_ms < f64::MAX {
            self.since_speech_ms += FRAME_MS;
        }
        self.track_floor(level_db);
        let mut speech = false;
        let room;
        let speech_threshold =
            (self.floor_db + self.options.speech_margin_db).max(self.options.speech_minimum_db);
        if level_db > speech_threshold {
            self.run = self.run.saturating_add(1);
            if self.run < self.options.speech_run_frames {
                self.run_power += power;
                room = false;
            } else {
                let first_run = self.run == self.options.speech_run_frames;
                let run_power =
                    if first_run { (self.run_power + power) / f64::from(self.run) } else { power };
                speech = self.is_foreground(run_power);
                if speech {
                    self.add_speech(run_power, if first_run { self.run } else { 1 });
                }
                room = !speech;
            }
        } else {
            if self.run > 0 && self.run < self.options.speech_run_frames {
                let run = self.run;
                self.add_noise(self.run_power / f64::from(run), run);
            }
            self.run = 0;
            self.run_power = 0.0;
            self.add_noise(power, 1);
            room = true;
        }
        self.track_room(room, speech);

        let o = &self.options;
        let previous_db = self.gain_db;
        let room_limit = if self.room_power >= 0.0 {
            0f64.max(o.max_room_noise_db - self.room_noise_db())
        } else if self.input_power >= 0.0 {
            self.gain_db
        } else {
            o.max_gain_db
        };
        if self.speech_ms >= o.min_speech_ms {
            let mut wanted = clamp_f64(o.target_db - self.speech_level_db(), 0.0, o.max_gain_db);
            wanted =
                wanted.min(0f64.max(o.max_output_noise_db - self.floor_db.max(self.noise_db())));
            wanted = wanted.min(room_limit);
            let settled = self.is_settled();
            let up = (if settled { o.rise_db_per_second } else { o.unsettled_db_per_second })
                * FRAME_MS
                / 1000.0;
            let down = (if settled { o.fall_db_per_second } else { o.unsettled_db_per_second })
                * FRAME_MS
                / 1000.0;
            if wanted > self.gain_db && speech {
                self.gain_db = wanted.min(self.gain_db + up);
            } else if wanted < self.gain_db {
                self.gain_db = wanted.max(self.gain_db - down);
            }
        } else if room_limit < self.gain_db {
            self.gain_db =
                room_limit.max(self.gain_db - o.room_limit_fall_db_per_second * FRAME_MS / 1000.0);
        }

        let from = to_linear(previous_db);
        let to = to_linear(self.gain_db);
        let ceiling = self.options.limiter_ceiling;
        let limit =
            if peak * from.max(to) > ceiling { ceiling / (peak * from.max(to)) } else { 1.0 };
        if from == 1.0 && to == 1.0 && limit == 1.0 {
            return;
        }
        let step = (to - from) / frame.len() as f32;
        let mut i = 0.0f32;
        for s in frame.iter_mut() {
            i += 1.0;
            *s *= (from + step * i) * limit;
        }
    }

    /// Before MinSpeechMs every voice counts as the person's: the first one heard sets the level.
    fn is_foreground(&self, power: f64) -> bool {
        self.speech_power < 0.0
            || self.speech_ms < self.options.min_speech_ms
            || power >= self.speech_power * self.foreground_ratio
    }

    fn track_room(&mut self, room: bool, speech: bool) {
        if speech {
            self.pending_room.fill(-1.0);
        }
        let candidate = room
            && self.input_power >= 0.0
            && self.elapsed_ms >= self.options.noise_settle_ms
            && self.since_speech_ms >= self.options.room_guard_ms;
        let len = self.pending_room.len();
        let Some(slot) = self.pending_room.get_mut(self.pending_next) else {
            self.pending_next = 0;
            return;
        };
        let committed = *slot;
        *slot = if candidate { self.input_power } else { -1.0 };
        self.pending_next = self.pending_next.saturating_add(1).checked_rem(len).unwrap_or(0);
        if committed < 0.0 {
            return;
        }
        if self.room_power < 0.0 {
            self.room_power = committed;
        } else {
            self.room_power += (committed - self.room_power)
                * (FRAME_MS / self.options.room_noise_time_constant_ms);
        }
    }

    fn add_speech(&mut self, power: f64, frames: u32) {
        self.speech_ms += f64::from(frames) * FRAME_MS;
        self.since_speech_ms = 0.0;
        if self.speech_power < 0.0 {
            self.speech_power = power;
        }
        for _ in 0..frames {
            self.speech_power +=
                (power - self.speech_power) * (FRAME_MS / self.options.level_time_constant_ms);
        }
    }

    /// Not before the floor has settled and not within NoiseAfterSpeechMs of speech.
    fn add_noise(&mut self, power: f64, frames: u32) {
        if self.elapsed_ms < self.options.noise_settle_ms
            || self.since_speech_ms < self.options.noise_after_speech_ms
        {
            return;
        }
        if self.noise_power < 0.0 {
            self.noise_power = power;
        }
        for _ in 0..frames {
            self.noise_power +=
                (power - self.noise_power) * (FRAME_MS / self.options.noise_time_constant_ms);
        }
    }

    /// Noise floor of the incoming (denoised) signal: falls quickly, rises slowly.
    fn track_floor(&mut self, level_db: f64) {
        if !self.floor_known {
            self.floor_known = true;
            self.floor_db = clamp_f64(level_db, -90.0, -20.0);
            return;
        }
        if level_db < self.floor_db {
            self.floor_db += (level_db - self.floor_db)
                * (1.0 - (-FRAME_MS / self.options.floor_fall_time_constant_ms).exp());
        } else {
            self.floor_db = level_db
                .min(self.floor_db + self.options.floor_rise_db_per_second * FRAME_MS / 1000.0);
        }
        self.floor_db = clamp_f64(self.floor_db, -90.0, -20.0);
    }
}

fn to_linear(db: f64) -> f32 {
    pow10(db / 20.0) as f32
}
