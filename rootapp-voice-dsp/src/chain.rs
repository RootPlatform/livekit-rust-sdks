use crate::aligner::{ring_samples, DenoiseFrameAligner};
use crate::auto_sensitivity::{AutoSensitivity, AutoSensitivityOptions};
use crate::gain_ramp::GainRamp;
use crate::noise_gate::{block_samples_for, NoiseGate};
use crate::sanitize::{self, FULL_SCALE};
use crate::speech_gate::{self, GateControl, SpeechDetection, SpeechGate};
use crate::transmit_gate::TransmitGate;
use crate::voice_gain::{VoiceGain, VoiceGainOptions};
use crate::{
    clamp_f64, AlignerStats, DenoiserOutput, DspCommands, DspOutputs, DspSettings, PreDenoise,
    PushToTalk, DENOISER_OUTPUT_FRAMES, DENOISER_STALL_FRAMES, MAX_DENOISER_WAIT_FRAMES, MAX_FRAME,
    MAX_RATE, MAX_VOICE_GAIN_DB, MIN_RATE, STARTUP_HOLD_FRAMES, TRANSMIT_FADE_MS,
};

/// 2^-15: FloatS16 to a ±1 signal (exact).
const TO_UNIT: f32 = 1.0 / 32_768.0;
/// 2^30: sums of squares of a ±1 signal back to FloatS16 units (exact).
const SUMSQ_TO_S16: f64 = 1_073_741_824.0;
/// Managed `NativeMicrophoneCapture.AutoMinOpenBlocks`.
pub const AUTO_MIN_OPEN_BLOCKS: u32 = 5;

fn frame_samples_for(rate: u32) -> usize {
    (rate / 100) as usize
}

fn max_backlog_for(rate: u32) -> usize {
    ((rate / 5) as usize).max(frame_samples_for(rate).saturating_mul(2))
}

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

fn push_to_talk(settings: &DspSettings) -> Option<bool> {
    match settings.push_to_talk {
        PushToTalk::VoiceActivity => None,
        PushToTalk::Held => Some(true),
        PushToTalk::Released => Some(false),
    }
}

/// Managed `MicPipelineHealth.Rms`.
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (sanitize::sum_squares(samples) / samples.len() as f64).sqrt() as f32
}

/// Managed `CallDiag.VoiceDb`.
fn voice_db(rms: f64) -> f32 {
    if rms > 0.0 {
        (20.0 * rms.log10()) as f32
    } else {
        f32::NEG_INFINITY
    }
}

/// Managed `CallDiag.blockPeakDb` with the chain's gate block.
fn block_peak_db(samples: &[f32], block: usize) -> f32 {
    let mut best = f32::NEG_INFINITY;
    for chunk in samples.chunks(block.max(1)) {
        let sum = sanitize::sum_squares(chunk);
        best = best.max(voice_db((sum / chunk.len() as f64).sqrt()));
    }
    best
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

/// The voice chain after the APM, channel 0 (managed `NativeMicrophoneCapture.processFrame`).
pub struct DspChain {
    max_rate: u32,
    /// The configured rate; 0 before the first supported frame.
    rate: u32,
    work: [f32; MAX_FRAME],
    denoised: Vec<f32>,
    pending: PendingCommands,
    /// Frame length a supported pre_denoise prepared `work` for.
    prepared: Option<usize>,
    voice_gain: VoiceGain,
    voice_gain_enabled: bool,
    start_gain_db: f64,
    voice_gain_db: f32,
    voice_gain_settled: bool,
    auto_sensitivity: AutoSensitivity,
    auto_sensitivity_active: bool,
    gate: NoiseGate,
    ramp: GainRamp,
    transmit: TransmitGate,
    aligner: DenoiseFrameAligner,
    aligner_in_use: bool,
    empty_outputs: u32,
    frames_since_open: u32,
    /// A pipeline ran or was installed (Ran or Missed) since the last reset or rate change: the
    /// startup hold no longer waits for the denoiser.
    denoiser_seen: bool,
    speech: SpeechGate,
    /// The detection mode the speech gate last ran in: a change starts its verdict over.
    speech_mode: SpeechDetection,
}

impl DspChain {
    /// Allocates everything for max_rate (clamped to MIN_RATE..=MAX_RATE). Never allocates
    /// afterwards.
    pub fn new(max_rate: u32) -> Self {
        let max_rate = max_rate.clamp(MIN_RATE, MAX_RATE);
        let max_frame = frame_samples_for(max_rate);
        let max_backlog = max_backlog_for(max_rate);
        Self {
            max_rate,
            rate: 0,
            work: [0.0; MAX_FRAME],
            denoised: vec![0.0; DENOISER_OUTPUT_FRAMES.saturating_mul(max_frame)],
            pending: PendingCommands::default(),
            prepared: None,
            voice_gain: VoiceGain::new(0.0, VoiceGainOptions::default()),
            voice_gain_enabled: false,
            start_gain_db: 0.0,
            voice_gain_db: 0.0,
            voice_gain_settled: false,
            auto_sensitivity: AutoSensitivity::new(AutoSensitivityOptions::default()),
            auto_sensitivity_active: false,
            gate: NoiseGate::new(max_rate, block_samples_for(max_rate)),
            ramp: GainRamp::new(1.0, ramp_samples_for(max_rate)),
            transmit: TransmitGate::new(fade_samples_for(max_rate), true),
            aligner: DenoiseFrameAligner::new(
                max_frame,
                max_backlog,
                ring_samples(max_frame, max_backlog),
            ),
            aligner_in_use: false,
            empty_outputs: 0,
            frames_since_open: 0,
            denoiser_seen: false,
            speech: SpeechGate::default(),
            speech_mode: SpeechDetection::Off,
        }
    }

    pub fn max_rate(&self) -> u32 {
        self.max_rate
    }

    fn supported(&self, rate: u32, len: usize) -> bool {
        (MIN_RATE..=self.max_rate).contains(&rate)
            && len > 0
            && len <= MAX_FRAME
            && len == frame_samples_for(rate)
    }

    /// Stream (re)start: the aligner, the stall count and the startup hold start over.
    fn flush_stream(&mut self) {
        self.aligner.reset();
        self.aligner_in_use = false;
        self.empty_outputs = 0;
        self.frames_since_open = 0;
        self.speech.reset();
    }

    /// A new rate (or a reset): flush, a fresh noise gate, ramps and fade at their targets. The
    /// installed pipeline runs at the old rate (NotInUse until one at the new rate arrives), so the
    /// startup hold waits for the denoiser again; a flush at the same rate keeps that.
    fn reconfigure(&mut self, rate: u32, settings: &DspSettings) {
        self.rate = rate;
        self.denoiser_seen = false;
        let threshold = self.gate.threshold();
        let min_open_blocks = self.gate.min_open_blocks();
        self.gate = NoiseGate::new(rate, block_samples_for(rate));
        self.gate.set_threshold(threshold);
        self.gate.set_min_open_blocks(min_open_blocks);
        self.ramp.snap(sanitize::input_volume(settings.input_volume), ramp_samples_for(rate));
        self.transmit.snap(fade_samples_for(rate), transmit_open(settings));
        self.aligner.configure(frame_samples_for(rate), max_backlog_for(rate));
        self.flush_stream();
    }

    /// Session start: reconfigure at the current rate, and the voice gain from 0 dB.
    fn reset_session(&mut self, rate: u32, settings: &DspSettings) {
        self.reconfigure(rate, settings);
        self.start_gain_db = 0.0;
        if self.voice_gain_enabled {
            self.voice_gain.reset(0.0);
        }
        self.voice_gain_db = 0.0;
        self.voice_gain_settled = false;
        self.auto_sensitivity_active = false;
    }

    fn reset_gain(&mut self, db: f32) {
        self.start_gain_db = clamp_f64(f64::from(db), 0.0, f64::from(MAX_VOICE_GAIN_DB));
        if self.voice_gain_enabled {
            self.voice_gain.reset(self.start_gain_db);
        }
        self.voice_gain_settled = false;
    }

    /// Managed `applyPendingControls` and `syncGainControl`.
    fn apply_controls(&mut self, settings: &DspSettings) {
        let ptt = push_to_talk(settings);
        if ptt != self.gate.push_to_talk() {
            self.gate.set_push_to_talk(ptt);
        }
        if !settings.auto_sensitivity {
            self.gate.set_threshold(sanitize::gate_threshold(settings.gate_threshold));
            self.gate.set_min_open_blocks(0);
        }
        if settings.auto_gain {
            if !self.voice_gain_enabled {
                self.voice_gain_enabled = true;
                self.voice_gain.reset(self.start_gain_db);
            }
        } else {
            self.voice_gain_enabled = false;
            self.voice_gain_settled = false;
        }
    }

    fn aligner_stats(&self) -> AlignerStats {
        let rate = self.rate.max(1);
        AlignerStats {
            primed: self.aligner.is_primed(),
            buffered_samples: u32::try_from(self.aligner.buffered_samples()).unwrap_or(u32::MAX),
            underruns: self.aligner.underruns(),
            padded_samples: self.aligner.padded_samples(),
            trimmed_samples: self.aligner.trimmed_samples(),
            drained_samples: self.aligner.drained_samples(),
            latency_ms: self.aligner.since_reset().latency_ms(rate) as f32,
            lifetime_latency_ms: self.aligner.lifetime().latency_ms(rate) as f32,
        }
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
            self.reset_session(rate, settings);
            out.reset = true;
            flushed = true;
        }
        if pending.flush {
            self.flush_stream();
            flushed = true;
        }
        if let Some(db) = pending.gain_reset {
            self.reset_gain(db);
        }
        out.flushed = flushed;

        self.apply_controls(settings);

        let Some(work) = self.work.get_mut(..frame.len()) else {
            self.prepared = None;
            return PreDenoise { flush_denoiser: flushed, supported: false };
        };
        for (w, &x) in work.iter_mut().zip(frame.iter()) {
            *w = x * TO_UNIT;
        }
        if self.voice_gain_enabled {
            self.voice_gain.observe_input(work);
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
        let prepared = match self.prepared.take() {
            Some(len) if len == frame.len() && settings.sample_rate == self.rate => Some(len),
            _ => None,
        };
        let Some(len) = prepared else {
            self.post_unsupported(frame, settings, out);
            return;
        };
        let open = transmit_open(settings);
        let Self {
            work,
            denoised: scratch,
            voice_gain,
            voice_gain_enabled,
            voice_gain_db,
            voice_gain_settled,
            auto_sensitivity,
            auto_sensitivity_active,
            gate,
            ramp,
            transmit,
            aligner,
            aligner_in_use,
            empty_outputs,
            frames_since_open,
            denoiser_seen,
            speech,
            speech_mode,
            ..
        } = self;
        let Some(w) = work.get_mut(..len) else {
            return;
        };

        let mut ran = false;
        match denoised {
            DenoiserOutput::NotInUse => {
                if *aligner_in_use {
                    aligner.reset();
                    *aligner_in_use = false;
                }
                *empty_outputs = 0;
            }
            DenoiserOutput::Missed => {
                *denoiser_seen = true;
            }
            DenoiserOutput::Ran { samples, restart } => {
                ran = true;
                *denoiser_seen = true;
                if restart {
                    aligner.reset();
                    *empty_outputs = 0;
                }
                *aligner_in_use = true;
                let take = samples.len().min(DENOISER_OUTPUT_FRAMES.saturating_mul(len));
                let pushed = match (samples.get(..take), scratch.get_mut(..take)) {
                    (Some(src), Some(dst)) => {
                        dst.copy_from_slice(src);
                        out.non_finite_samples =
                            out.non_finite_samples.saturating_add(sanitize::sanitize(dst));
                        for s in dst.iter_mut() {
                            *s *= TO_UNIT;
                        }
                        dst
                    }
                    _ => &mut [],
                };
                if pushed.is_empty() {
                    *empty_outputs = empty_outputs.saturating_add(1);
                } else {
                    *empty_outputs = 0;
                    aligner.push(pushed);
                }
                aligner.pop(w);
            }
        }
        out.denoised_sumsq = sanitize::sum_squares(w) * SUMSQ_TO_S16;
        out.denoiser_ran = ran;
        out.denoiser_stalled = *empty_outputs >= DENOISER_STALL_FRAMES;

        let denoised_rms = rms(w);
        let mut gain_db = 0.0;
        if *voice_gain_enabled {
            voice_gain.process(w);
            gain_db = voice_gain.gain_db();
            *voice_gain_db = gain_db as f32;
            *voice_gain_settled = voice_gain.is_settled();
        }

        let input_rms = rms(w);
        let detection = if gate.push_to_talk().is_none() {
            settings.speech_detection
        } else {
            SpeechDetection::Off
        };
        if detection != *speech_mode {
            speech.reset();
            *speech_mode = detection;
        }
        let probability = match detection {
            SpeechDetection::Off => None,
            SpeechDetection::Ring | SpeechDetection::Gate => {
                speech_gate::probability(settings.speech_probability)
            }
        };
        let gate_audio = probability.is_some() && detection == SpeechDetection::Gate;
        let level_open = gate.envelope() > 0.0;
        match probability {
            Some(p) => speech.observe(p, level_open, gate_audio),
            None => speech.reset(),
        }
        if settings.auto_sensitivity {
            if !*auto_sensitivity_active {
                auto_sensitivity.reset();
                *auto_sensitivity_active = true;
            }
            auto_sensitivity.set_open_minimum_db(f64::from(sanitize::auto_sensitivity_minimum_db(
                settings.auto_sensitivity_minimum_db,
            )));
            // The level gate's own state: the speech gate never moves the threshold.
            let auto_open = gate.level_envelope() > 0.0;
            gate.set_threshold(auto_sensitivity.update(
                denoised_rms,
                gain_db,
                auto_open,
                settings.capture_silent,
            ));
            gate.set_min_open_blocks(AUTO_MIN_OPEN_BLOCKS);
        } else {
            *auto_sensitivity_active = false;
        }

        *frames_since_open = frames_since_open.saturating_add(1);
        let held = *frames_since_open <= STARTUP_HOLD_FRAMES
            || (settings.denoiser_wanted
                && !*denoiser_seen
                && *frames_since_open <= MAX_DENOISER_WAIT_FRAMES);
        if held {
            w.fill(0.0);
        }
        out.gate_in_block_peak_db = block_peak_db(w, gate.block_samples());
        out.gate_in_sumsq = sanitize::sum_squares(w) * SUMSQ_TO_S16;

        let released = gate.push_to_talk() == Some(false);
        let above = input_rms > gate.threshold();
        let control = match probability {
            Some(p) if gate_audio => speech.control(p, input_rms, gate.threshold(), level_open),
            _ => GateControl::LEVEL,
        };
        gate.process_controlled(w, !released, control);
        if gate_audio {
            speech.after_gate(gate.envelope());
        }

        ramp.set_target(sanitize::input_volume(settings.input_volume));
        ramp.apply(w);
        for s in w.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
        let transmitting = transmit.apply(w, open);
        for (x, &v) in frame.iter_mut().zip(w.iter()) {
            *x = v * FULL_SCALE;
        }

        let threshold = gate.threshold();
        let envelope = gate.envelope();
        let gate_speaking = gate.is_speaking();
        let speech_confirmed = probability.is_some() && speech.confirmed();
        let speaking =
            if probability.is_some() { gate_speaking && speech_confirmed } else { gate_speaking };
        out.sent_sumsq = sanitize::sum_squares(frame);
        out.gate_in_rms = input_rms;
        out.threshold = threshold;
        out.envelope = envelope;
        out.gate_speaking = gate_speaking;
        out.speaking = speaking && open;
        out.speech_probability = probability.unwrap_or(-1.0);
        out.speech = speech_confirmed;
        out.speech_closed = control.fast_close || (!control.allow_open && above);
        out.gate_open = envelope > 0.0;
        out.gate_input_above = input_rms > threshold;
        out.held = held;
        out.transmit_open = open;
        out.transmitting = transmitting;
        out.voice_gain_db = *voice_gain_db;
        out.voice_gain_settled = *voice_gain_settled;
        out.unsupported = false;
        out.aligner = self.aligner_stats();
    }

    /// The input volume and the transmit gate (without a fade) only. The noise gate hears a frame
    /// of silence at the configured rate, so its speaking state runs out instead of freezing;
    /// everything else reports its last state.
    fn post_unsupported(
        &mut self,
        frame: &mut [f32],
        settings: &DspSettings,
        out: &mut DspOutputs,
    ) {
        let open = transmit_open(settings);
        let passed = sanitize::sum_squares(frame);
        self.ramp.set_target(sanitize::input_volume(settings.input_volume));
        self.ramp.apply(frame);
        sanitize::clamp_full_scale(frame);
        let transmitting = self.transmit.apply_hard(frame, open);
        if self.rate != 0 {
            if let Some(silence) = self.work.get_mut(..frame_samples_for(self.rate)) {
                silence.fill(0.0);
                self.gate.process_gated(silence, false);
            }
        }
        out.sent_sumsq = sanitize::sum_squares(frame);
        out.denoised_sumsq = passed;
        out.gate_in_sumsq = passed;
        out.gate_in_rms = 0.0;
        out.gate_in_block_peak_db = f32::NEG_INFINITY;
        out.threshold = self.gate.threshold();
        out.envelope = self.gate.envelope();
        self.speech.reset();
        out.gate_speaking = self.gate.is_speaking();
        out.speaking = out.gate_speaking && open;
        out.speech_probability = -1.0;
        out.speech = false;
        out.speech_closed = false;
        out.gate_open = false;
        out.gate_input_above = false;
        out.held = false;
        out.transmit_open = open;
        out.transmitting = transmitting;
        out.voice_gain_db = self.voice_gain_db;
        out.voice_gain_settled = self.voice_gain_settled;
        out.denoiser_ran = false;
        out.denoiser_stalled = false;
        out.aligner = self.aligner_stats();
        out.unsupported = true;
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
