use crate::{clamp_f64, pow10, speech_gate};

const FRAME_MS: f64 = 10.0;
/// The room estimate a committed frame is clipped against never counts as quieter than this
/// (-90 dB), so a room that was digital silence can still be learned.
const MIN_ROOM_POWER: f64 = 1e-9;

/// The voice gain's tunables a host can set (RootAudioControl 17-22; managed `VoiceGainOptions`
/// defaults, which the desktop sends). A non-finite field is the default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceGainTuning {
    pub max_gain_db: f32,
    pub target_db: f32,
    pub rise_db_per_second: f32,
    pub fall_db_per_second: f32,
    pub level_time_constant_ms: f32,
    pub min_speech_probability: f32,
}

impl VoiceGainTuning {
    /// Nothing set: every field the default.
    pub const UNSET: Self = Self {
        max_gain_db: f32::NAN,
        target_db: f32::NAN,
        rise_db_per_second: f32::NAN,
        fall_db_per_second: f32::NAN,
        level_time_constant_ms: f32::NAN,
        min_speech_probability: f32::NAN,
    };
    pub const DEFAULT: Self = Self {
        max_gain_db: 12.0,
        target_db: -25.0,
        rise_db_per_second: 3.0,
        fall_db_per_second: 2.0,
        level_time_constant_ms: 4000.0,
        min_speech_probability: 0.5,
    };
    /// The lowest value of each field a host can set.
    pub const MIN: Self = Self {
        max_gain_db: 0.0,
        target_db: -40.0,
        rise_db_per_second: 0.5,
        fall_db_per_second: 0.5,
        level_time_constant_ms: 500.0,
        min_speech_probability: 0.0,
    };
    /// The highest value of each field a host can set.
    pub const MAX: Self = Self {
        max_gain_db: crate::MAX_VOICE_GAIN_DB,
        target_db: -10.0,
        rise_db_per_second: 20.0,
        fall_db_per_second: 20.0,
        level_time_constant_ms: 20_000.0,
        min_speech_probability: 1.0,
    };

    /// Each field clamped to MIN..=MAX, a non-finite one the default.
    pub fn sanitized(self) -> Self {
        let field = |value: f32, default: f32, min: f32, max: f32| {
            if value.is_finite() {
                value.clamp(min, max)
            } else {
                default
            }
        };
        let (d, lo, hi) = (Self::DEFAULT, Self::MIN, Self::MAX);
        Self {
            max_gain_db: field(self.max_gain_db, d.max_gain_db, lo.max_gain_db, hi.max_gain_db),
            target_db: field(self.target_db, d.target_db, lo.target_db, hi.target_db),
            rise_db_per_second: field(
                self.rise_db_per_second,
                d.rise_db_per_second,
                lo.rise_db_per_second,
                hi.rise_db_per_second,
            ),
            fall_db_per_second: field(
                self.fall_db_per_second,
                d.fall_db_per_second,
                lo.fall_db_per_second,
                hi.fall_db_per_second,
            ),
            level_time_constant_ms: field(
                self.level_time_constant_ms,
                d.level_time_constant_ms,
                lo.level_time_constant_ms,
                hi.level_time_constant_ms,
            ),
            min_speech_probability: field(
                self.min_speech_probability,
                d.min_speech_probability,
                lo.min_speech_probability,
                hi.min_speech_probability,
            ),
        }
    }
}

impl Default for VoiceGainTuning {
    fn default() -> Self {
        Self::UNSET
    }
}

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
    pub room_burst_db: f64,
    pub speech_margin_db: f64,
    pub speech_minimum_db: f64,
    pub min_speech_probability: f64,
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
    pub limiter_release_db_per_second: f64,
}

impl Default for VoiceGainOptions {
    fn default() -> Self {
        let tuning = VoiceGainTuning::DEFAULT;
        Self {
            target_db: f64::from(tuning.target_db),
            max_gain_db: f64::from(tuning.max_gain_db),
            max_output_noise_db: -60.0,
            max_room_noise_db: -50.0,
            foreground_margin_db: 12.0,
            room_noise_time_constant_ms: 2000.0,
            room_guard_ms: 200.0,
            room_burst_db: 6.0,
            speech_margin_db: 10.0,
            speech_minimum_db: -62.0,
            min_speech_probability: f64::from(tuning.min_speech_probability),
            level_time_constant_ms: f64::from(tuning.level_time_constant_ms),
            min_speech_ms: 500.0,
            settled_speech_ms: 3000.0,
            rise_db_per_second: f64::from(tuning.rise_db_per_second),
            fall_db_per_second: f64::from(tuning.fall_db_per_second),
            unsettled_db_per_second: 6.0,
            floor_fall_time_constant_ms: 300.0,
            floor_rise_db_per_second: 2.0,
            speech_run_frames: 3,
            noise_time_constant_ms: 2000.0,
            noise_settle_ms: 1000.0,
            noise_after_speech_ms: 250.0,
            limiter_ceiling: 0.9,
            limiter_release_db_per_second: 100.0,
        }
    }
}

/// Managed `VoiceGain`: automatic gain for the denoised voice, one `process` per 10 ms frame.
#[derive(Clone, Debug)]
pub struct VoiceGain {
    options: VoiceGainOptions,
    room_burst_ratio: f64,
    limiter_release: f32,
    pending_room: Vec<f64>,
    gain_db: f64,
    floor_db: f64,
    level_sum: f64,
    level_weight: f64,
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
    limiter: f32,
}

impl VoiceGain {
    pub fn new(start_gain_db: f64, options: VoiceGainOptions) -> Self {
        let pending = ((options.room_guard_ms / FRAME_MS) as usize).max(1);
        let mut gain = Self {
            room_burst_ratio: pow10(options.room_burst_db / 10.0),
            limiter_release: pow10(options.limiter_release_db_per_second * FRAME_MS / 1000.0 / 20.0)
                as f32,
            options,
            pending_room: vec![-1.0; pending],
            gain_db: 0.0,
            floor_db: -90.0,
            level_sum: 0.0,
            level_weight: 0.0,
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
            limiter: 1.0,
        };
        gain.reset(start_gain_db);
        gain
    }

    pub fn options(&self) -> &VoiceGainOptions {
        &self.options
    }

    /// The host's tunables, sanitised here: they apply from the next frame and never allocate. A
    /// gain over a lowered maximum comes down at the fall rate; a reset clamps to it.
    pub fn set_tuning(&mut self, tuning: &VoiceGainTuning) {
        let t = tuning.sanitized();
        let o = &mut self.options;
        o.max_gain_db = f64::from(t.max_gain_db);
        o.target_db = f64::from(t.target_db);
        o.rise_db_per_second = f64::from(t.rise_db_per_second);
        o.fall_db_per_second = f64::from(t.fall_db_per_second);
        o.level_time_constant_ms = f64::from(t.level_time_constant_ms);
        o.min_speech_probability = f64::from(t.min_speech_probability);
    }

    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    /// The talker's level: the average frame level (dBFS) of their speech over about
    /// LevelTimeConstantMs of it, each frame weighted by its speech probability (1 without one).
    pub fn speech_level_db(&self) -> f64 {
        if self.level_weight > 0.0 {
            self.level_sum / self.level_weight
        } else {
            f64::NEG_INFINITY
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

    /// The peak limiter's gain on the last frame (1: not limiting).
    pub fn limiter_gain(&self) -> f32 {
        self.limiter
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
        self.level_sum = 0.0;
        self.level_weight = 0.0;
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
        self.limiter = 1.0;
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

    /// A frame without a speech probability.
    pub fn process(&mut self, frame: &mut [f32]) {
        self.process_with_probability(frame, -1.0);
    }

    /// `speech_probability`: the speech detector's for this frame's input, 0..1 (above 1 is 1);
    /// negative or non-finite when there is none, and level alone decides what is speech.
    pub fn process_with_probability(&mut self, frame: &mut [f32], speech_probability: f32) {
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
        let probability = speech_gate::probability(speech_probability);
        let voiced = probability.map(|p| f64::from(p) >= self.options.min_speech_probability);
        let weight = probability.map_or(1.0, f64::from);

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
                let run_db = if first_run {
                    let mean = (self.run_power + power) / f64::from(self.run);
                    10.0 * mean.max(1e-12).log10()
                } else {
                    level_db
                };
                let foreground = self.is_foreground(run_db);
                speech = foreground && voiced != Some(false);
                if speech {
                    self.add_speech(run_db, weight, if first_run { self.run } else { 1 });
                }
                room = !foreground;
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
        let settled = self.is_settled();
        let up = (if settled { o.rise_db_per_second } else { o.unsettled_db_per_second })
            * FRAME_MS
            / 1000.0;
        let down = (if settled { o.fall_db_per_second } else { o.unsettled_db_per_second })
            * FRAME_MS
            / 1000.0;
        let room_limit = if self.room_power >= 0.0 {
            0f64.max(o.max_room_noise_db - self.room_noise_db())
        } else if self.input_power >= 0.0 {
            self.gain_db
        } else {
            o.max_gain_db
        };
        if self.speech_ms >= o.min_speech_ms && self.level_weight > 0.0 {
            let mut wanted = clamp_f64(o.target_db - self.speech_level_db(), 0.0, o.max_gain_db);
            wanted =
                wanted.min(0f64.max(o.max_output_noise_db - self.floor_db.max(self.noise_db())));
            wanted = wanted.min(room_limit);
            if wanted > self.gain_db && speech {
                self.gain_db = wanted.min(self.gain_db + up);
            } else if wanted < self.gain_db {
                self.gain_db = wanted.max(self.gain_db - down);
            }
        } else if room_limit < self.gain_db {
            self.gain_db = room_limit.max(self.gain_db - down);
        }

        let from = to_linear(previous_db);
        let to = to_linear(self.gain_db);
        let ceiling = self.options.limiter_ceiling;
        let needed =
            if peak * from.max(to) > ceiling { ceiling / (peak * from.max(to)) } else { 1.0 };
        let limit_from = self.limiter;
        let limit_to = needed.min(limit_from * self.limiter_release).min(1.0);
        self.limiter = limit_to;
        if from == 1.0 && to == 1.0 && limit_from == 1.0 && limit_to == 1.0 {
            return;
        }
        let step = (to - from) / frame.len() as f32;
        let limit_start = if limit_to < limit_from { limit_to } else { limit_from };
        let limit_step = (limit_to - limit_start) / frame.len() as f32;
        let mut i = 0.0f32;
        for s in frame.iter_mut() {
            i += 1.0;
            *s *= (from + step * i) * (limit_start + limit_step * i);
        }
    }

    /// Before MinSpeechMs every voice counts as the person's: the first one heard sets the level.
    fn is_foreground(&self, level_db: f64) -> bool {
        self.level_weight <= 0.0
            || self.speech_ms < self.options.min_speech_ms
            || level_db >= self.speech_level_db() - self.options.foreground_margin_db
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
            let clipped =
                committed.min(self.room_power.max(MIN_ROOM_POWER) * self.room_burst_ratio);
            self.room_power +=
                (clipped - self.room_power) * (FRAME_MS / self.options.room_noise_time_constant_ms);
        }
    }

    fn add_speech(&mut self, level_db: f64, weight: f64, frames: u32) {
        self.speech_ms += f64::from(frames) * FRAME_MS;
        self.since_speech_ms = 0.0;
        let leak = 1.0 - FRAME_MS / self.options.level_time_constant_ms;
        for _ in 0..frames {
            self.level_sum = self.level_sum * leak + level_db * weight;
            self.level_weight = self.level_weight * leak + weight;
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
