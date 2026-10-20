use crate::aligner::{ring_samples, DenoiseFrameAligner};
use crate::auto_sensitivity::{self, AutoSensitivity, AutoSensitivityOptions};
use crate::frame_delay::{FrameDelay, FrameTag};
use crate::gain_ramp::GainRamp;
use crate::noise_gate::{block_samples_for, NoiseGate};
use crate::sanitize::{self, FULL_SCALE};
use crate::speech_gate::{
    self, GateControl, SpeechDetection, SpeechGate, QUIET_RMS, SPEECH_OFF, STEADY_SPEECH_FRAMES,
};
use crate::transmit_gate::TransmitGate;
use crate::voice_gain::{GainLimit, VoiceGain, VoiceGainOptions};
use crate::{
    clamp_f64, Agc2Tuning, Aligned, AlignerStats, DenoiserOutput, DspCommands, DspOutputs,
    DspSettings, GainEngine, PreDenoise, PushToTalk, ThresholdTerm, DENOISER_OUTPUT_FRAMES,
    DENOISER_STALL_FRAMES, MAX_DENOISER_WAIT_FRAMES, MAX_FRAME, MAX_LOOKAHEAD_FRAMES, MAX_RATE,
    MAX_VOICE_GAIN_DB, MIN_RATE, STARTUP_HOLD_FRAMES, TRANSMIT_FADE_MS,
};

/// 2^-15: FloatS16 to a ±1 signal (exact).
const TO_UNIT: f32 = 1.0 / 32_768.0;
/// 2^30: sums of squares of a ±1 signal back to FloatS16 units (exact).
const SUMSQ_TO_S16: f64 = 1_073_741_824.0;
/// Managed `NativeMicrophoneCapture.AutoMinOpenBlocks`.
pub const AUTO_MIN_OPEN_BLOCKS: u32 = 5;
/// -15 dB in power: the denoiser erased a frame this far under its input.
const DENOISER_ERASED_RATIO: f64 = 0.031_622_776_601_683_79;
/// The lowest open threshold reported, dB.
const LOWEST_THRESHOLD_DB: f64 = -140.0;
const PRE_LEVELS: usize = STEADY_SPEECH_FRAMES as usize;

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

/// The voice gain's view for the stats (DspOutputs voice_gain_room_db etc.).
#[derive(Clone, Copy, Debug)]
struct GainView {
    room_db: f32,
    speech_level_db: f32,
    floor_db: f32,
    limit: GainLimit,
}

impl GainView {
    const NONE: Self = Self {
        room_db: f32::NAN,
        speech_level_db: f32::NAN,
        floor_db: f32::NAN,
        limit: GainLimit::None,
    };

    fn of(gain: &VoiceGain, limit: GainLimit) -> Self {
        let level = gain.speech_level_db();
        Self {
            room_db: gain.room_noise_db() as f32,
            speech_level_db: if level.is_finite() { level as f32 } else { f32::NAN },
            floor_db: gain.floor_db() as f32,
            limit,
        }
    }

    fn write(&self, out: &mut DspOutputs) {
        out.voice_gain_room_db = self.room_db;
        out.voice_gain_speech_level_db = self.speech_level_db;
        out.voice_gain_floor_db = self.floor_db;
        out.voice_gain_limit = self.limit;
    }
}

/// The open threshold before the gain and what set it, for the stats.
#[derive(Clone, Copy, Debug)]
struct ThresholdView {
    floor_db: f32,
    open_db: f32,
    term: ThresholdTerm,
}

impl ThresholdView {
    const NONE: Self = Self { floor_db: f32::NAN, open_db: f32::NAN, term: ThresholdTerm::None };

    fn auto(auto: &AutoSensitivity, gain_db: f64) -> Self {
        Self {
            floor_db: auto.floor_db() as f32,
            open_db: auto.open_db_at(gain_db).max(LOWEST_THRESHOLD_DB) as f32,
            term: if auto.floor_sets_open_at(gain_db) {
                ThresholdTerm::Floor
            } else {
                ThresholdTerm::Minimum
            },
        }
    }

    fn manual(threshold: f32, gain_db: f64) -> Self {
        Self {
            floor_db: f32::NAN,
            open_db: (auto_sensitivity::to_db(threshold) - gain_db).max(LOWEST_THRESHOLD_DB) as f32,
            term: ThresholdTerm::Manual,
        }
    }

    fn write(&self, out: &mut DspOutputs) {
        out.auto_sensitivity_floor_db = self.floor_db;
        out.open_threshold_db = self.open_db;
        out.threshold_term = self.term;
    }
}

/// What holds the host's gain (External): its maximum, or its noise cap over the floor the chain's
/// voice gain measures, within 1 dB; the talker's level otherwise, once the voice gain heard enough
/// speech to have one.
fn external_limit(gain: &VoiceGain, applied_db: f64, tuning: &Agc2Tuning) -> GainLimit {
    let t = tuning.sanitized();
    let noise_cap = f64::from(t.max_output_noise_dbfs) - gain.floor_db();
    if applied_db >= f64::from(t.max_gain_db) - 1.0 {
        GainLimit::Max
    } else if applied_db >= noise_cap - 1.0 {
        GainLimit::Noise
    } else if gain.speech_ms() < gain.options().min_speech_ms {
        GainLimit::None
    } else {
        GainLimit::Target
    }
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
    /// Frame length post_denoise_align left the aligned voice in `work` for.
    aligned: Option<usize>,
    voice_gain: VoiceGain,
    voice_gain_enabled: bool,
    start_gain_db: f64,
    voice_gain_db: f32,
    voice_gain_settled: bool,
    gain_engine: GainEngine,
    gain_view: GainView,
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
    /// The speech gate's look-ahead: the frames the gate's decisions run ahead of what it gates.
    delay: FrameDelay,
    /// Frames in a row the detector rated SPEECH_OFF or more.
    steady_frames: u32,
    /// Mean squares (FloatS16) of the last PRE_LEVELS pre-denoise frames.
    pre_levels: [f64; PRE_LEVELS],
    pre_level_next: usize,
    /// post_denoise_align ran the denoiser this frame, and its aligned output's mean square.
    aligned_ran: bool,
    aligned_level: f64,
    threshold_view: ThresholdView,
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
            aligned: None,
            voice_gain: VoiceGain::new(0.0, VoiceGainOptions::default()),
            voice_gain_enabled: false,
            start_gain_db: 0.0,
            voice_gain_db: 0.0,
            voice_gain_settled: false,
            gain_engine: GainEngine::Legacy,
            gain_view: GainView::NONE,
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
            delay: FrameDelay::new(MAX_LOOKAHEAD_FRAMES, max_frame),
            steady_frames: 0,
            pre_levels: [0.0; PRE_LEVELS],
            pre_level_next: 0,
            aligned_ran: false,
            aligned_level: 0.0,
            threshold_view: ThresholdView::NONE,
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

    /// Stream (re)start: the aligner, the stall count, the startup hold, the speech verdict and the
    /// look-ahead start over (what the look-ahead held is dropped).
    fn flush_stream(&mut self) {
        self.aligner.reset();
        self.aligner_in_use = false;
        self.empty_outputs = 0;
        self.frames_since_open = 0;
        self.speech.reset();
        self.steady_frames = 0;
        self.delay.clear();
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
        self.gain_view = GainView::NONE;
        self.auto_sensitivity_active = false;
        self.threshold_view = ThresholdView::NONE;
    }

    fn reset_gain(&mut self, db: f32) {
        self.start_gain_db = clamp_f64(f64::from(db), 0.0, f64::from(MAX_VOICE_GAIN_DB));
        if self.voice_gain_enabled {
            self.voice_gain.reset(self.start_gain_db);
            self.gain_view = GainView::NONE;
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
        if let Some(level) = self.pre_levels.get_mut(self.pre_level_next) {
            *level = out.apm_out_sumsq / frame.len() as f64;
        }
        self.pre_level_next =
            self.pre_level_next.saturating_add(1).checked_rem(PRE_LEVELS).unwrap_or(0);
        // Before the commands, so a gain reset in this frame is clamped to this frame's maximum.
        self.voice_gain.set_tuning(&settings.voice_gain);

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
    /// post_denoise_align, then post_gain with the chain's own voice gain.
    pub fn post_denoise(
        &mut self,
        frame: &mut [f32],
        denoised: DenoiserOutput<'_>,
        settings: &DspSettings,
        out: &mut DspOutputs,
    ) {
        let aligned = self.post_denoise_align(frame, denoised, settings, out);
        let settings = DspSettings { gain_engine: GainEngine::Legacy, ..*settings };
        self.post_gain(frame, aligned.pre_gain_rms, f32::NAN, &settings, out);
    }

    /// The denoise stage: the denoiser's output through the aligner (or the frame as it is when no
    /// denoiser is in use), written to `frame` (FloatS16) before any gain. A host's own gain
    /// (GainEngine::External) runs on it next; post_gain finishes the frame. Called without a
    /// matching pre_denoise, the frame is left alone and post_gain treats it as unsupported.
    pub fn post_denoise_align(
        &mut self,
        frame: &mut [f32],
        denoised: DenoiserOutput<'_>,
        settings: &DspSettings,
        out: &mut DspOutputs,
    ) -> Aligned {
        self.aligned = None;
        self.aligned_ran = false;
        let prepared = match self.prepared.take() {
            Some(len) if len == frame.len() && settings.sample_rate == self.rate => Some(len),
            _ => None,
        };
        let Some(len) = prepared else {
            return Aligned::default();
        };
        let Self {
            work,
            denoised: scratch,
            aligner,
            aligner_in_use,
            empty_outputs,
            denoiser_seen,
            ..
        } = self;
        let Some(w) = work.get_mut(..len) else {
            return Aligned::default();
        };

        let mut ran = false;
        let mut skipped = false;
        match denoised {
            DenoiserOutput::Skipped => {
                skipped = true;
                w.fill(0.0);
            }
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
        out.denoiser_skipped = skipped;
        out.denoiser_stalled = *empty_outputs >= DENOISER_STALL_FRAMES;
        let pre_gain_rms = rms(w);
        for (x, &v) in frame.iter_mut().zip(w.iter()) {
            *x = v * FULL_SCALE;
        }
        self.aligned = Some(len);
        self.aligned_ran = ran;
        self.aligned_level = out.denoised_sumsq / len as f64;
        Aligned { supported: true, pre_gain_rms }
    }

    /// Everything from the voice gain on, on the frame post_denoise_align wrote (same settings).
    /// pre_gain_rms is what it returned. With GainEngine::External the host already applied its
    /// gain to `frame` and gain_db is that gain (clamped to 0..50, non-finite: 0); the chain's own
    /// voice gain runs on a copy of the aligned frame from before it, for the room, the talker's
    /// level and settled. Legacy: the chain's own voice gain boosts the frame and gain_db is
    /// ignored.
    pub fn post_gain(
        &mut self,
        frame: &mut [f32],
        pre_gain_rms: f32,
        gain_db: f32,
        settings: &DspSettings,
        out: &mut DspOutputs,
    ) {
        let aligned = match self.aligned.take() {
            Some(len) if len == frame.len() && settings.sample_rate == self.rate => Some(len),
            _ => None,
        };
        let Some(len) = aligned else {
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
            gain_engine,
            gain_view,
            auto_sensitivity,
            auto_sensitivity_active,
            gate,
            ramp,
            transmit,
            frames_since_open,
            denoiser_seen,
            speech,
            speech_mode,
            delay,
            steady_frames,
            pre_levels,
            aligned_ran,
            aligned_level,
            threshold_view,
            ..
        } = self;
        let Some(w) = work.get_mut(..len) else {
            return;
        };
        let external = settings.gain_engine == GainEngine::External;
        if external && *voice_gain_enabled {
            if let Some(observed) = scratch.get_mut(..len) {
                observed.copy_from_slice(w);
                voice_gain.process_with_probability(observed, settings.speech_probability);
            }
        }
        for (s, &x) in w.iter_mut().zip(frame.iter()) {
            *s = x * TO_UNIT;
        }

        *gain_engine = settings.gain_engine;
        let mut applied_db = 0.0;
        if *voice_gain_enabled {
            let limit = match settings.gain_engine {
                GainEngine::Legacy => {
                    voice_gain.process_with_probability(w, settings.speech_probability);
                    applied_db = voice_gain.gain_db();
                    voice_gain.limit()
                }
                GainEngine::External => {
                    applied_db = if gain_db.is_finite() {
                        clamp_f64(f64::from(gain_db), 0.0, f64::from(Agc2Tuning::MAX.max_gain_db))
                    } else {
                        0.0
                    };
                    external_limit(voice_gain, applied_db, &settings.agc2)
                }
            };
            *voice_gain_db = applied_db as f32;
            *voice_gain_settled = voice_gain.is_settled();
            *gain_view = GainView::of(voice_gain, limit);
        }

        let input_rms = rms(w);
        let detection = settings.speech_detection;
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
        // Push-to-talk leaves the audio to the key; the verdict keeps running, so speech that
        // continues when the key mode ends is already confirmed.
        let gate_audio = probability.is_some()
            && detection == SpeechDetection::Gate
            && gate.push_to_talk().is_none();
        let level_open = gate.envelope() > 0.0;
        match probability {
            Some(p) => speech.observe(p),
            None => speech.reset(),
        }
        *steady_frames = match probability {
            Some(p) if p >= SPEECH_OFF => steady_frames.saturating_add(1),
            _ => 0,
        };
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
                pre_gain_rms,
                applied_db,
                auto_open,
                settings.capture_silent,
            ));
            gate.set_min_open_blocks(AUTO_MIN_OPEN_BLOCKS);
            *threshold_view = ThresholdView::auto(auto_sensitivity, applied_db);
        } else {
            *auto_sensitivity_active = false;
            *threshold_view = ThresholdView::manual(gate.threshold(), applied_db);
        }

        *frames_since_open = frames_since_open.saturating_add(1);
        let held = *frames_since_open <= STARTUP_HOLD_FRAMES
            || (settings.denoiser_wanted
                && !*denoiser_seen
                && *frames_since_open <= MAX_DENOISER_WAIT_FRAMES);
        if held {
            w.fill(0.0);
        }

        let speech_confirmed = probability.is_some() && speech.confirmed();
        let push_to_talk = gate.push_to_talk().is_some();
        let steady = speech_confirmed && *steady_frames >= STEADY_SPEECH_FRAMES && !held;
        let erased = steady && open && *aligned_ran && {
            let quietest = pre_levels.iter().fold(f64::INFINITY, |m, &v| m.min(v));
            *aligned_level <= quietest * DENOISER_ERASED_RATIO
        };
        let tag = FrameTag {
            held,
            open,
            speech: steady && !erased && !push_to_talk && input_rms > QUIET_RMS,
        };

        // The look-ahead: the gate decides on this frame and applies the decision to the frame
        // `lookahead` frames older. Starting it (or changing it) sends that many frames of silence
        // first; ending it drops what it held.
        let lookahead = if gate_audio {
            sanitize::speech_lookahead_frames(settings.speech_lookahead_ms)
        } else {
            0
        };
        if lookahead != delay.frames() || (lookahead > 0 && delay.frame_len() != len) {
            delay.restart(lookahead, len, FrameTag { speech: false, ..tag });
        }
        let mut sent_tag = tag;
        let (measure, sent): (Option<&[f32]>, &mut [f32]) = match scratch.get_mut(..len) {
            Some(apply) if delay.frames() > 0 => {
                sent_tag = delay.exchange(w, apply, tag);
                (Some(&*w), apply)
            }
            _ => (None, w),
        };
        out.gate_in_block_peak_db = block_peak_db(sent, gate.block_samples());
        out.gate_in_sumsq = sanitize::sum_squares(sent) * SUMSQ_TO_S16;

        let released = gate.push_to_talk() == Some(false);
        let above = input_rms > gate.threshold();
        let control = match probability {
            Some(_) if gate_audio => speech.control(input_rms, level_open),
            _ => GateControl::LEVEL,
        };
        let shut_before = gate.envelope() <= 0.0;
        match measure {
            Some(measure) => gate.process_split(measure, sent, !released, control),
            None => gate.process_controlled(sent, !released, control),
        }
        if gate_audio {
            speech.after_gate(gate.envelope());
        }

        let sent_open = open && sent_tag.open;
        ramp.set_target(sanitize::input_volume(settings.input_volume));
        ramp.apply(sent);
        for s in sent.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
        let transmitting = transmit.apply(sent, sent_open);
        for (x, &v) in frame.iter_mut().zip(sent.iter()) {
            *x = v * FULL_SCALE;
        }

        let threshold = gate.threshold();
        let envelope = gate.envelope();
        let gate_speaking = gate.is_speaking();
        out.sent_sumsq = sanitize::sum_squares(frame);
        out.gate_in_rms = input_rms;
        out.threshold = threshold;
        out.envelope = envelope;
        out.gate_speaking = gate_speaking;
        out.speaking = sent_open && !sent_tag.held && (gate_speaking || out.sent_sumsq > 0.0);
        out.speech_probability = probability.unwrap_or(-1.0);
        out.speech = speech_confirmed;
        out.speech_closed = control.fast_close || (!control.allow_open && above);
        out.gate_open = envelope > 0.0;
        out.gate_input_above = input_rms > threshold;
        out.held = sent_tag.held;
        out.transmit_open = sent_open;
        out.transmitting = transmitting;
        out.voice_gain_db = *voice_gain_db;
        out.voice_gain_settled = *voice_gain_settled;
        out.gain_engine = *gain_engine;
        gain_view.write(out);
        out.speech_lookahead_ms = u32::try_from(delay.frames().saturating_mul(10)).unwrap_or(0);
        out.speech_denoiser_erased = erased;
        out.speech_gate_cut =
            sent_tag.speech && sent_open && !push_to_talk && shut_before && envelope <= 0.0;
        threshold_view.write(out);
        out.unsupported = false;
        out.aligner = self.aligner_stats();
    }

    /// The input volume and the transmit gate (without a fade) only. The noise gate hears a frame
    /// of silence at the configured rate, so its speaking state runs out instead of freezing; the
    /// look-ahead drops what it held; everything else reports its last state.
    fn post_unsupported(
        &mut self,
        frame: &mut [f32],
        settings: &DspSettings,
        out: &mut DspOutputs,
    ) {
        self.delay.clear();
        self.steady_frames = 0;
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
        out.speaking = transmitting && out.sent_sumsq > 0.0;
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
        out.gain_engine = self.gain_engine;
        self.gain_view.write(out);
        out.denoiser_ran = false;
        out.denoiser_skipped = false;
        out.denoiser_stalled = false;
        out.speech_lookahead_ms = 0;
        out.speech_denoiser_erased = false;
        out.speech_gate_cut = false;
        self.threshold_view.write(out);
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
