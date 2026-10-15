/// libwebrtc's AGC2 (GainController2, adaptive digital) as the platform engine's voice gain
/// (`GainEngine::External`): the settings a host can set (RootAudioControl 24-27). The host runs
/// AGC2 between `post_denoise_align` and `post_gain`; the chain only reads these to tell which limit
/// holds the gain. A non-finite field is the default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Agc2Tuning {
    pub headroom_db: f32,
    pub max_gain_db: f32,
    pub max_gain_change_db_per_second: f32,
    pub max_output_noise_dbfs: f32,
}

impl Agc2Tuning {
    /// Nothing set: every field the default.
    pub const UNSET: Self = Self {
        headroom_db: f32::NAN,
        max_gain_db: f32::NAN,
        max_gain_change_db_per_second: f32::NAN,
        max_output_noise_dbfs: f32::NAN,
    };
    /// Chrome's desktop settings, but at most 30 dB (Chrome: 50).
    pub const DEFAULT: Self = Self {
        headroom_db: 5.0,
        max_gain_db: 30.0,
        max_gain_change_db_per_second: 6.0,
        max_output_noise_dbfs: -50.0,
    };
    pub const MIN: Self = Self {
        headroom_db: 0.0,
        max_gain_db: 1.0,
        max_gain_change_db_per_second: 1.0,
        max_output_noise_dbfs: -80.0,
    };
    pub const MAX: Self = Self {
        headroom_db: 20.0,
        max_gain_db: 50.0,
        max_gain_change_db_per_second: 20.0,
        max_output_noise_dbfs: -30.0,
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
            headroom_db: field(self.headroom_db, d.headroom_db, lo.headroom_db, hi.headroom_db),
            max_gain_db: field(self.max_gain_db, d.max_gain_db, lo.max_gain_db, hi.max_gain_db),
            max_gain_change_db_per_second: field(
                self.max_gain_change_db_per_second,
                d.max_gain_change_db_per_second,
                lo.max_gain_change_db_per_second,
                hi.max_gain_change_db_per_second,
            ),
            max_output_noise_dbfs: field(
                self.max_output_noise_dbfs,
                d.max_output_noise_dbfs,
                lo.max_output_noise_dbfs,
                hi.max_output_noise_dbfs,
            ),
        }
    }

    /// AGC2's starting gain for a gain reset of `db`: 0..max_gain_db (non-finite: 0).
    pub fn initial_gain_db(&self, db: f32) -> f32 {
        let max = self.sanitized().max_gain_db;
        if db.is_finite() {
            db.clamp(0.0, max)
        } else {
            0.0
        }
    }
}

impl Default for Agc2Tuning {
    fn default() -> Self {
        Self::UNSET
    }
}
