use crate::gain_ramp::GainRamp;
use crate::sanitize::{self, FULL_SCALE};
use crate::transmit_gate::TransmitGate;
use crate::{
    DenoiserOutput, DspCommands, DspOutputs, DspSettings, PreDenoise, PushToTalk, MAX_FRAME,
    MAX_RATE, MIN_RATE, TRANSMIT_FADE_MS,
};

/// 2^-15: FloatS16 to a ±1 signal (exact).
const TO_UNIT: f32 = 1.0 / 32_768.0;
/// 2^30: sums of squares of a ±1 signal back to FloatS16 units (exact).
const SUMSQ_TO_S16: f64 = 1_073_741_824.0;

fn ramp_samples_for(rate: u32) -> u32 {
    (rate / 50).max(1)
}

fn fade_samples_for(rate: u32) -> u32 {
    let fade = u64::from(rate).saturating_mul(u64::from(TRANSMIT_FADE_MS)) / 1000;
    u32::try_from(fade).unwrap_or(u32::MAX).max(1)
}

fn transmit_open(settings: &DspSettings) -> bool {
    !settings.mic_muted && settings.push_to_talk != PushToTalk::Released
}

#[derive(Clone, Copy, Debug, Default)]
struct PendingCommands {
    reset: bool,
    flush: bool,
    gain_reset: Option<f32>,
}

impl PendingCommands {
    /// In order: a reset wipes an earlier gain reset, a later one survives it.
    fn add(&mut self, commands: &DspCommands) {
        if commands.reset {
            self.reset = true;
            self.gain_reset = None;
        }
        self.flush |= commands.flush;
        if commands.gain_reset && commands.gain_reset_db.is_finite() {
            self.gain_reset = Some(commands.gain_reset_db);
        }
    }
}

pub struct DspChain {
    max_rate: u32,
    /// The configured rate; 0 before the first supported frame.
    rate: u32,
    work: [f32; MAX_FRAME],
    pending: PendingCommands,
    /// Frame length a supported pre_denoise prepared `work` for.
    prepared: Option<usize>,
    ramp: GainRamp,
    transmit: TransmitGate,
}

impl DspChain {
    /// Allocates everything for max_rate (clamped to MIN_RATE..=MAX_RATE). Never allocates
    /// afterwards.
    pub fn new(max_rate: u32) -> Self {
        let max_rate = max_rate.clamp(MIN_RATE, MAX_RATE);
        Self {
            max_rate,
            rate: 0,
            work: [0.0; MAX_FRAME],
            pending: PendingCommands::default(),
            prepared: None,
            ramp: GainRamp::new(1.0, ramp_samples_for(max_rate)),
            transmit: TransmitGate::new(fade_samples_for(max_rate), true),
        }
    }

    pub fn max_rate(&self) -> u32 {
        self.max_rate
    }

    fn supported(&self, rate: u32, len: usize) -> bool {
        (MIN_RATE..=self.max_rate).contains(&rate)
            && len > 0
            && len <= MAX_FRAME
            && len == (rate / 100) as usize
    }

    /// Ramps and the transmit gate at the target, sized for `rate`.
    fn reconfigure(&mut self, rate: u32, settings: &DspSettings) {
        self.rate = rate;
        self.ramp.snap(sanitize::input_volume(settings.input_volume), ramp_samples_for(rate));
        self.transmit.snap(fade_samples_for(rate), transmit_open(settings));
    }

    /// frame: channel 0, FloatS16. Sanitised in place; otherwise unchanged, so the caller hands
    /// exactly this to the SDK.
    pub fn pre_denoise(
        &mut self,
        frame: &mut [f32],
        settings: &DspSettings,
        commands: &DspCommands,
        out: &mut DspOutputs,
    ) -> PreDenoise {
        out.non_finite_samples = sanitize::sanitize(frame);
        out.apm_out_sumsq = sanitize::sum_squares(frame);
        out.samples = u32::try_from(frame.len()).unwrap_or(u32::MAX);
        out.reset = false;
        out.flushed = false;
        self.pending.add(commands);
        let rate = settings.sample_rate;
        if !self.supported(rate, frame.len()) {
            self.prepared = None;
            out.unsupported = true;
            return PreDenoise { flush_denoiser: false, supported: false };
        }
        out.unsupported = false;

        let mut flushed = false;
        if rate != self.rate {
            self.reconfigure(rate, settings);
            flushed = true;
        }
        let pending = std::mem::take(&mut self.pending);
        if pending.reset {
            self.reconfigure(rate, settings);
            out.reset = true;
            flushed = true;
        }
        if pending.flush {
            flushed = true;
        }
        out.flushed = flushed;

        let Some(work) = self.work.get_mut(..frame.len()) else {
            self.prepared = None;
            return PreDenoise { flush_denoiser: flushed, supported: false };
        };
        for (w, &x) in work.iter_mut().zip(frame.iter()) {
            *w = x * TO_UNIT;
        }
        self.prepared = Some(frame.len());
        PreDenoise { flush_denoiser: flushed, supported: true }
    }

    /// Same frame and settings as the preceding pre_denoise. Writes the processed FloatS16 frame.
    /// Called without a matching pre_denoise, the frame is treated as unsupported.
    pub fn post_denoise(
        &mut self,
        frame: &mut [f32],
        denoised: DenoiserOutput<'_>,
        settings: &DspSettings,
        out: &mut DspOutputs,
    ) {
        let _ = denoised;
        let prepared = self.prepared.take();
        let rate_matches = settings.sample_rate == self.rate;
        let work = match prepared {
            Some(len) if len == frame.len() && rate_matches => self.work.get_mut(..len),
            _ => None,
        };
        let Some(w) = work else {
            self.post_unsupported(frame, settings, out);
            return;
        };

        let open = transmit_open(settings);
        out.unsupported = false;
        let denoised_sumsq = sanitize::sum_squares(w) * SUMSQ_TO_S16;
        out.denoised_sumsq = denoised_sumsq;
        out.gate_in_sumsq = denoised_sumsq;
        out.gate_in_rms = 0.0;
        out.gate_in_block_peak_db = f32::NEG_INFINITY;
        out.held = false;
        out.gate_open = false;
        out.gate_input_above = false;
        out.denoiser_ran = false;
        out.denoiser_stalled = false;

        self.ramp.set_target(sanitize::input_volume(settings.input_volume));
        self.ramp.apply(w);
        for sample in w.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
        out.transmitting = self.transmit.apply(w, open);
        for (x, &v) in frame.iter_mut().zip(w.iter()) {
            *x = v * FULL_SCALE;
        }
        out.sent_sumsq = sanitize::sum_squares(frame);
        out.transmit_open = open;
        out.speaking = out.gate_speaking && open;
    }

    fn post_unsupported(
        &mut self,
        frame: &mut [f32],
        settings: &DspSettings,
        out: &mut DspOutputs,
    ) {
        let open = transmit_open(settings);
        sanitize::clamp_full_scale(frame);
        self.transmit.apply_hard(frame, open);
        let sumsq = sanitize::sum_squares(frame);
        out.unsupported = true;
        out.sent_sumsq = sumsq;
        out.transmit_open = open;
        out.transmitting = open;
        out.speaking = out.gate_speaking && open;
        out.held = false;
        out.gate_open = false;
        out.gate_input_above = false;
        out.denoiser_ran = false;
    }

    /// pre_denoise + post_denoise(NotInUse).
    pub fn process(
        &mut self,
        frame: &mut [f32],
        settings: &DspSettings,
        commands: &DspCommands,
        out: &mut DspOutputs,
    ) {
        self.pre_denoise(frame, settings, commands, out);
        self.post_denoise(frame, DenoiserOutput::NotInUse, settings, out);
    }
}
