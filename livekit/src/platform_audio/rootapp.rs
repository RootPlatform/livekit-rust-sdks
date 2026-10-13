// Copyright 2026 LiveKit, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Types of the RootApp native audio engine (device requests, controls and
//! stats).

use std::fmt;

use libwebrtc::peer_connection_factory::native::{
    AudioDeviceOutcome, AudioDeviceRequestOutcome, MonitorResult, NativeAudioControl,
    NativeAudioStats, NativeCaptureFormat, NativeDenoiserCompletion, NativeDenoiserState,
    NativeMixSourceLevel, NativeReturnPipelines, NativeReturnedPipeline, NativeSpeaking,
    ADM_RESULT_NOT_ATTEMPTED,
};
pub use libwebrtc::peer_connection_factory::native::{AudioDeviceSelection, EffectPlay};

use super::{AudioError, AudioResult};

/// Why [`set_monitor`](super::PlatformAudio::set_monitor) changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorError {
    /// This build has no capture processor (mobile).
    NotSupported,
    /// A call sends from the platform microphone; the mic test waits for it.
    Sending,
    /// The platform audio device module is not acquired.
    NotAcquired,
    /// The platform audio device module could not be created or initialized.
    NoDevice,
}

impl fmt::Display for MonitorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotSupported => "not supported by this build",
            Self::Sending => "refused: a call is sending from the platform microphone",
            Self::NotAcquired => "platform audio is not acquired",
            Self::NoDevice => "platform audio has no device module",
        })
    }
}

impl std::error::Error for MonitorError {}

pub(crate) fn monitor_result(result: MonitorResult) -> Result<(), MonitorError> {
    match result {
        MonitorResult::Applied => Ok(()),
        MonitorResult::NotSupported => Err(MonitorError::NotSupported),
        MonitorResult::Sending => Err(MonitorError::Sending),
        MonitorResult::NotAcquired => Err(MonitorError::NotAcquired),
        MonitorResult::NoDevice => Err(MonitorError::NoDevice),
    }
}

/// Slider volumes (input, master, tile) are linear 0..2.
pub const MAX_VOLUME: f32 = 2.0;
/// The app's voice gain only boosts, up to 18 dB.
pub const MAX_VOICE_GAIN_DB: f32 = 18.0;

/// A slider volume made safe for the audio thread: NaN is 0 (silent), the
/// rest is clamped to 0..[`MAX_VOLUME`].
pub fn sanitize_volume(volume: f32) -> f32 {
    if volume.is_nan() {
        0.0
    } else {
        volume.clamp(0.0, MAX_VOLUME)
    }
}

fn sanitize_unit(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

/// State of the capture gate's push-to-talk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushToTalk {
    /// No push-to-talk: voice activity decides (Phase 1: the gate is open).
    VoiceActivity,
    /// Key held: the gate is forced open.
    Held,
    /// Key up: nothing is sent.
    Released,
}

impl PushToTalk {
    /// The wire value (0, 1, 2). Unknown values close the gate.
    pub fn from_wire(value: u32) -> Self {
        match value {
            0 => Self::VoiceActivity,
            1 => Self::Held,
            _ => Self::Released,
        }
    }

    pub fn to_wire(self) -> u32 {
        match self {
            Self::VoiceActivity => 0,
            Self::Held => 1,
            Self::Released => 2,
        }
    }
}

/// What the native speech detector's verdict qualifies (DSP chain; push-to-talk
/// ignores it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechDetection {
    /// The noise gate alone decides (the managed engine's behaviour).
    Off,
    /// The ring and the speaking flag need confirmed speech; the audio follows
    /// the noise gate.
    Ring,
    /// As `Ring`, and the noise gate also closes on what the detector rejects.
    Gate,
}

impl SpeechDetection {
    /// The wire value (0, 1, 2). Unknown values are off.
    pub fn from_wire(value: u32) -> Self {
        match value {
            1 => Self::Ring,
            2 => Self::Gate,
            _ => Self::Off,
        }
    }

    pub fn to_wire(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Ring => 1,
            Self::Gate => 2,
        }
    }
}

/// Whether this build's native engine runs the echo model. Until it does, a
/// control that sets [`RootAudioControl::echo_model`] is answered as not
/// supported and [`RootAudioStats::echo_model`] stays `None`.
pub const ECHO_MODEL_AVAILABLE: bool = false;

/// The echo model (DTLN-aec-128 as AEC3's neural residual echo estimator):
/// which of the APM's two variants runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoModel {
    /// AEC3 alone, exactly as builds without the model.
    Off,
    /// The model on every output device.
    On,
    /// The playout device's kind decides at every open; an echo path AEC3
    /// measures while it runs alone turns the model on for that device.
    Auto,
}

impl EchoModel {
    /// The wire value (0, 1, 2). Unknown values are off.
    pub fn from_wire(value: u32) -> Self {
        match value {
            1 => Self::On,
            2 => Self::Auto,
            _ => Self::Off,
        }
    }

    pub fn to_wire(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::On => 1,
            Self::Auto => 2,
        }
    }
}

/// What the APM runs: `Starting` and `Stopping` until the next audio frame
/// switches to the variant native decided on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoModelState {
    Off,
    On,
    Starting,
    Stopping,
}

impl EchoModelState {
    pub fn from_wire(value: u32) -> Self {
        match value {
            1 => Self::On,
            2 => Self::Starting,
            3 => Self::Stopping,
            _ => Self::Off,
        }
    }

    pub fn to_wire(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::On => 1,
            Self::Starting => 2,
            Self::Stopping => 3,
        }
    }
}

/// Why native decided on its echo model state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoModelReason {
    ModeOff,
    ModeOn,
    /// Auto, by [`OutputDeviceKind`].
    DeviceKind,
    /// Auto: AEC3 measured an echo path on the current output device.
    MeasuredEcho,
    /// The CPU kill switch: off until the session ends.
    KillSwitch,
    /// The model or its APM could not be built.
    Unavailable,
}

impl EchoModelReason {
    pub fn from_wire(value: u32) -> Self {
        match value {
            1 => Self::ModeOn,
            2 => Self::DeviceKind,
            3 => Self::MeasuredEcho,
            4 => Self::KillSwitch,
            5 => Self::Unavailable,
            _ => Self::ModeOff,
        }
    }

    pub fn to_wire(self) -> u32 {
        match self {
            Self::ModeOff => 0,
            Self::ModeOn => 1,
            Self::DeviceKind => 2,
            Self::MeasuredEcho => 3,
            Self::KillSwitch => 4,
            Self::Unavailable => 5,
        }
    }
}

/// The platform playout device's kind, read from the OS when it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputDeviceKind {
    /// No platform playout device opened since the session started.
    NoDevice,
    Unknown,
    Speakers,
    LineOut,
    /// HDMI or DisplayPort audio.
    DisplayAudio,
    Headphones,
    Headset,
    Handset,
    /// Any Bluetooth device, whatever its form factor.
    Bluetooth,
}

impl OutputDeviceKind {
    pub fn from_wire(value: u32) -> Self {
        match value {
            0 => Self::NoDevice,
            2 => Self::Speakers,
            3 => Self::LineOut,
            4 => Self::DisplayAudio,
            5 => Self::Headphones,
            6 => Self::Headset,
            7 => Self::Handset,
            8 => Self::Bluetooth,
            _ => Self::Unknown,
        }
    }

    pub fn to_wire(self) -> u32 {
        match self {
            Self::NoDevice => 0,
            Self::Unknown => 1,
            Self::Speakers => 2,
            Self::LineOut => 3,
            Self::DisplayAudio => 4,
            Self::Headphones => 5,
            Self::Headset => 6,
            Self::Handset => 7,
            Self::Bluetooth => 8,
        }
    }

    /// The kinds Auto runs the model on from the device's first frame.
    pub fn is_loudspeaker(self) -> bool {
        matches!(self, Self::Speakers | Self::LineOut | Self::DisplayAudio)
    }
}

/// The echo model's state and costs (the FFI `RootAudioStats` fields 114-124).
/// Counters are cumulative for the engine's life; the inference mean and
/// maximum are since the previous read (NaN and 0 without an inference).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EchoModelStats {
    pub mode: EchoModel,
    pub state: EchoModelState,
    pub reason: EchoModelReason,
    pub output_device_kind: OutputDeviceKind,
    pub swaps: u64,
    pub inferences: u64,
    pub inference_mean_us: f32,
    pub inference_max_us: u64,
    /// The last complete 1000-inference window's 99th percentile; NaN before one.
    pub inference_p99_us: f32,
    pub measured_echo: bool,
    pub kill_switch: bool,
}

/// Live control of the native audio engine. `None` leaves a setting as it is.
///
/// Take effect now: `mic_muted` and `push_to_talk` (the capture gate fades
/// out over 5 ms and zeroes every frame while muted or the key is up, and
/// fades in over 5 ms; both fields of one control change together),
/// `input_volume` (20 ms ramp), `master_volume` and `deafened` (remote audio
/// only; effects ignore both), and `echo_cancellation` /
/// `webrtc_noise_suppression` (the APM policy, applied at most once a second
/// because each toggle re-initializes the echo canceller or the noise
/// suppressor, and with the DSP chain at a pause in speech, at most 3 s later).
///
/// The DSP chain's settings, read on its next frame: `denoise_power` (the
/// installed Effects SDK pipeline's power), `auto_gain`, `gate_threshold`,
/// `auto_sensitivity`, the `gain_reset_db` command and `denoiser_wanted` (the
/// host's noise suppression mode is the Effects SDK: the startup hold waits
/// for the denoiser and native WebRTC NS covers a denoiser that stops or does
/// not arrive) and `speech_detection` (off until set, and after every session
/// start). Builds without the chain only store them. `echo_model` is off until
/// set and after every session start; without [`ECHO_MODEL_AVAILABLE`] it is
/// not supported.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RootAudioControl {
    pub mic_muted: Option<bool>,
    pub push_to_talk: Option<PushToTalk>,
    /// Linear 0..2.
    pub input_volume: Option<f32>,
    /// The output volume slider / 100, linear 0..2; remote audio plays at
    /// VolumeCurve(tile x master).
    pub master_volume: Option<f32>,
    pub deafened: Option<bool>,
    pub echo_cancellation: Option<bool>,
    pub webrtc_noise_suppression: Option<bool>,
    /// 0..1.
    pub denoise_power: Option<f32>,
    pub auto_gain: Option<bool>,
    /// Linear RMS, 0..1.
    pub gate_threshold: Option<f32>,
    pub auto_sensitivity: Option<bool>,
    /// 0..[`MAX_VOICE_GAIN_DB`].
    pub gain_reset_db: Option<f32>,
    pub denoiser_wanted: Option<bool>,
    pub speech_detection: Option<SpeechDetection>,
    pub echo_model: Option<EchoModel>,
}

impl RootAudioControl {
    /// Every value finite and in range: volumes clamp to 0..2 and NaN is 0,
    /// the denoise power and gate threshold clamp to 0..1 and NaN is 0, and a
    /// non-finite gain reset is dropped.
    pub fn sanitized(&self) -> Self {
        Self {
            input_volume: self.input_volume.map(sanitize_volume),
            master_volume: self.master_volume.map(sanitize_volume),
            denoise_power: self.denoise_power.map(sanitize_unit),
            gate_threshold: self.gate_threshold.map(sanitize_unit),
            gain_reset_db: self
                .gain_reset_db
                .filter(|db| db.is_finite())
                .map(|db| db.clamp(0.0, MAX_VOICE_GAIN_DB)),
            ..*self
        }
    }

    /// False when a set field has nothing in this build to act on it that the
    /// native side does not answer for itself: `echo_model` without
    /// [`ECHO_MODEL_AVAILABLE`].
    pub fn supported_by_this_build(&self) -> bool {
        self.echo_model.is_none() || ECHO_MODEL_AVAILABLE
    }

    pub(crate) fn to_native(&self) -> NativeAudioControl {
        let c = self.sanitized();
        NativeAudioControl {
            has_mic_muted: c.mic_muted.is_some(),
            mic_muted: c.mic_muted.unwrap_or_default(),
            has_push_to_talk: c.push_to_talk.is_some(),
            push_to_talk: c.push_to_talk.map(PushToTalk::to_wire).unwrap_or_default(),
            has_input_volume: c.input_volume.is_some(),
            input_volume: c.input_volume.unwrap_or_default(),
            has_master_volume: c.master_volume.is_some(),
            master_volume: c.master_volume.unwrap_or_default(),
            has_deafened: c.deafened.is_some(),
            deafened: c.deafened.unwrap_or_default(),
            has_echo_cancellation: c.echo_cancellation.is_some(),
            echo_cancellation: c.echo_cancellation.unwrap_or_default(),
            has_noise_suppression: c.webrtc_noise_suppression.is_some(),
            noise_suppression: c.webrtc_noise_suppression.unwrap_or_default(),
            has_denoise_power: c.denoise_power.is_some(),
            denoise_power: c.denoise_power.unwrap_or_default(),
            has_auto_gain: c.auto_gain.is_some(),
            auto_gain: c.auto_gain.unwrap_or_default(),
            has_gate_threshold: c.gate_threshold.is_some(),
            gate_threshold: c.gate_threshold.unwrap_or_default(),
            has_auto_sensitivity: c.auto_sensitivity.is_some(),
            auto_sensitivity: c.auto_sensitivity.unwrap_or_default(),
            has_gain_reset_db: c.gain_reset_db.is_some(),
            gain_reset_db: c.gain_reset_db.unwrap_or_default(),
            has_denoiser_wanted: c.denoiser_wanted.is_some(),
            denoiser_wanted: c.denoiser_wanted.unwrap_or_default(),
            has_speech_detection: c.speech_detection.is_some(),
            speech_detection: c.speech_detection.map(SpeechDetection::to_wire).unwrap_or_default(),
        }
    }
}

/// A finished [`set_denoiser`](super::PlatformAudio::set_denoiser) request of
/// `owner`; `error` is `None` once native holds the pipeline. On an error
/// native no longer holds it: `pipeline` names it when nothing else will
/// (superseded, or the audio thread did not let go), and is 0 when it comes
/// back another way (a ReturnPipelines response, or the returned list when the
/// last session ended).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenoiserCompletion {
    pub owner: u64,
    pub async_id: u64,
    pub error: Option<String>,
    pub pipeline: u64,
}

impl From<NativeDenoiserCompletion> for DenoiserCompletion {
    fn from(value: NativeDenoiserCompletion) -> Self {
        let error = (!value.error.is_empty()).then_some(value.error);
        Self {
            owner: value.owner,
            async_id: value.async_id,
            pipeline: if error.is_some() { value.pipeline } else { 0 },
            error,
        }
    }
}

/// An Effects SDK pipeline native no longer holds (replaced, removed, or the
/// last session ended), for the owner whose request installed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReturnedPipeline {
    pub owner: u64,
    pub pipeline: u64,
}

impl From<NativeReturnedPipeline> for ReturnedPipeline {
    fn from(value: NativeReturnedPipeline) -> Self {
        Self { owner: value.owner, pipeline: value.pipeline }
    }
}

/// Everything [`return_denoiser_pipelines`](super::PlatformAudio::return_denoiser_pipelines)
/// took back, each once. `error` says the installed pipeline could not be
/// removed in time (it stays installed and is not listed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReturnedPipelines {
    pub pipelines: Vec<u64>,
    pub error: Option<String>,
}

impl From<NativeReturnPipelines> for ReturnedPipelines {
    fn from(value: NativeReturnPipelines) -> Self {
        Self { pipelines: value.pipelines, error: (!value.error.is_empty()).then_some(value.error) }
    }
}

/// The Effects SDK slot: the installed pipeline's rate (0 none), why it is
/// bypassed (0 none, 1 rate mismatch, 2 output stalled, 3 slow), and whether
/// native WebRTC NS covers it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DenoiserState {
    pub installed: bool,
    pub rate: u32,
    pub bypass_reason: u32,
    pub webrtc_ns_fallback: bool,
}

impl From<NativeDenoiserState> for DenoiserState {
    fn from(value: NativeDenoiserState) -> Self {
        Self {
            installed: value.installed,
            rate: value.rate,
            bypass_reason: value.bypass_reason,
            webrtc_ns_fallback: value.webrtc_ns_fallback,
        }
    }
}

/// The local user's published speaking state (the noise gate's speaking while
/// the transmit gate is open) and how often it changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalSpeaking {
    pub speaking: bool,
    pub seq: u64,
}

impl From<NativeSpeaking> for LocalSpeaking {
    fn from(value: NativeSpeaking) -> Self {
        Self { speaking: value.speaking, seq: value.seq }
    }
}

/// What the capture post-processor runs at: the APM's full-band rate and the
/// channel count of its buffer. `seq` moves whenever either changes (0 before
/// the first processed frame).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureFormat {
    pub seq: u64,
    pub sample_rate: u32,
    pub channels: u32,
}

impl From<NativeCaptureFormat> for CaptureFormat {
    fn from(value: NativeCaptureFormat) -> Self {
        Self { seq: value.seq, sample_rate: value.sample_rate, channels: value.channels }
    }
}

/// Result of a device request per direction; `None` for a direction the request
/// did not cover.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioDeviceResults {
    pub recording: Option<AudioResult<()>>,
    pub playout: Option<AudioResult<()>>,
}

impl AudioDeviceResults {
    pub(crate) fn from_outcome(
        outcome: Option<AudioDeviceRequestOutcome>,
        (recording, playout): (bool, bool),
    ) -> Self {
        let Some(outcome) = outcome else {
            let stopped = || Err(AudioError::OperationFailed("audio device thread stopped".into()));
            return Self { recording: recording.then(stopped), playout: playout.then(stopped) };
        };
        Self {
            recording: outcome.recording.map(device_result),
            playout: outcome.playout.map(device_result),
        }
    }
}

fn device_result(outcome: AudioDeviceOutcome) -> AudioResult<()> {
    let failed = |result: i32| result != 0 && result != ADM_RESULT_NOT_ATTEMPTED;
    if !outcome.found {
        return Err(AudioError::DeviceNotFound);
    }
    if outcome.attempted && failed(outcome.stop_result) {
        return Err(AudioError::OperationFailed(format!(
            "device stop failed ({}); retried natively",
            outcome.stop_result
        )));
    }
    if outcome.attempted && failed(outcome.init_result) {
        return Err(AudioError::OperationFailed(format!(
            "device init failed ({})",
            outcome.init_result
        )));
    }
    if outcome.attempted && failed(outcome.start_result) {
        return Err(AudioError::OperationFailed(format!(
            "device start failed ({})",
            outcome.start_result
        )));
    }
    Ok(())
}

/// One playout mixer source (a remote audio stream): its audio as it reached the
/// mixer, decoded and before its gain (a muted frame counts as silence), as a sum
/// of squares in int16 units over `samples`, cumulative since the source was
/// added. `source_id` tells a stream added again under the same SSRC from the
/// earlier one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixSourceLevel {
    pub ssrc: u32,
    pub source_id: u64,
    pub sumsq: f64,
    pub samples: u64,
}

impl From<&NativeMixSourceLevel> for MixSourceLevel {
    fn from(level: &NativeMixSourceLevel) -> Self {
        Self {
            ssrc: level.ssrc,
            source_id: level.source_id,
            sumsq: level.sumsq,
            samples: level.samples,
        }
    }
}

/// Counters of the native audio engine, field for field the FFI `RootAudioStats`.
///
/// Counters are cumulative since the engine started (take deltas); levels are sums
/// of squares in int16 units with their sample counts; durations are microseconds;
/// `*_max_gap_us` is the maximum since the previous read. Last Init/Start results
/// are ADM return codes: 0 success, 1 not attempted yet, -10000 selected device
/// not present, other negative values ADM errors. Fields of stages this build does
/// not have stay 0, `false`, or NaN for dB values and echo statistics, and the
/// optional ones are `None` (no DSP chain, Effects SDK slot or APM decorator).
/// `gate_in_block_peak_db`, `envelope` and `denoiser_process_max_us` are
/// maxima since the previous read. `echo_cancellation_active` and
/// `webrtc_ns_active` are what the APM has applied; the echo statistics are the
/// APM's (NaN while it has none, and `aec_delay_ms` -1).
#[derive(Debug, Clone, PartialEq)]
pub struct RootAudioStats {
    // Device IO (transport tap on the platform ADM).
    pub capture_callbacks: u64,
    pub render_callbacks: u64,
    pub capture_max_gap_us: u64,
    pub render_max_gap_us: u64,
    pub last_capture_age_us: u64,
    pub last_render_age_us: u64,
    pub recording: bool,
    pub playing: bool,
    pub recording_init_result: i32,
    pub recording_start_result: i32,
    pub playout_init_result: i32,
    pub playout_start_result: i32,
    pub capture_rate: u32,
    pub capture_channels: u32,
    pub capture_raw_sumsq: f64,
    pub capture_raw_samples: u64,
    pub playout_delay_ms: u32,
    pub recording_delay_ms: u32,

    // Mixer.
    pub mix_calls: u64,
    pub mix_max_gap_us: u64,
    pub mix_sources: u32,
    pub mix_ssrcs: Vec<u32>,
    pub mix_out_sumsq: f64,
    pub mix_out_samples: u64,
    pub effects_played: u64,
    pub effects_dropped: u64,
    /// Each mixer source's level before its gain; `None` when the build does
    /// not count them.
    pub mix_source_levels: Option<Vec<MixSourceLevel>>,

    // Capture processing.
    pub processed_frames: u64,
    pub process_rate: u32,
    pub process_channels: u32,
    pub apm_out_sumsq: f64,
    pub denoised_sumsq: f64,
    pub gate_in_sumsq: f64,
    pub sent_sumsq: f64,
    pub stage_samples: u64,
    pub gate_in_block_peak_db: f32,
    pub threshold: f32,
    pub envelope: f32,
    pub gate_speaking: bool,
    pub speaking_seq: u64,
    pub voice_gain_db: f32,
    pub voice_gain_settled: bool,
    pub gate_open_frames: u64,

    // Denoiser.
    pub denoiser_installed: bool,
    pub denoiser_frames: u64,
    pub denoiser_misses: u64,
    pub denoiser_empty_outputs: u64,
    pub denoiser_process_max_us: u64,
    pub denoiser_slow_frames: u64,
    pub denoiser_bypass_reason: u32,
    pub denoiser_notifications: u64,

    // Echo canceller.
    pub erl_db: f64,
    pub erle_db: f64,
    pub aec_delay_ms: i32,
    pub divergent_filter_fraction: f64,
    pub echo_cancellation_active: bool,
    pub webrtc_ns_active: bool,

    // Real-time rule checks. Locks taken and libwebrtc log lines raised on device
    // threads; device callbacks and mixes that found another one in flight (a
    // device thread the ADM failed to stop) and were answered with silence;
    // devices that failed to stop.
    pub audio_thread_lock_violations: u64,
    pub audio_thread_logs_dropped: u64,
    pub overlapping_callbacks: u64,
    pub device_stop_failures: u64,

    // DSP chain.
    pub speaking: Option<bool>,
    pub gate_input_above_frames: Option<u64>,
    pub startup_hold_frames: Option<u64>,
    pub transmit_frames: Option<u64>,
    pub unsupported_frames: Option<u64>,
    pub non_finite_samples: Option<u64>,
    pub dsp_resets: Option<u64>,
    pub dsp_flushes: Option<u64>,
    pub gate_open_delay_us: Option<u64>,
    pub ns_gap_frames: Option<u64>,

    // Denoiser details (Effects SDK slot); the histogram is empty without it.
    pub denoiser_rate: Option<u32>,
    pub denoiser_installs: Option<u64>,
    pub denoiser_bypassed_frames: Option<u64>,
    pub denoiser_process_us_buckets: Vec<u64>,
    pub denoiser_other_notifications: Option<u64>,
    pub denoiser_latency_ms: Option<f32>,
    pub denoiser_buffered_samples: Option<u32>,
    pub denoiser_underruns: Option<u64>,
    pub denoiser_padded_samples: Option<u64>,
    pub denoiser_trimmed_samples: Option<u64>,
    pub denoiser_drained_samples: Option<u64>,
    pub denoiser_frames_until_first_output: Option<i32>,
    pub webrtc_ns_fallback: Option<bool>,
    /// Frames processed while the fallback was decided on but WebRTC NS was
    /// not applied yet.
    pub ns_fallback_pending_frames: Option<u64>,

    // APM policy (APM decorator).
    pub apm_policy_applies: Option<u64>,
    pub apm_policy_deferred: Option<u64>,
    pub apm_policy_forced: Option<u64>,
    pub apm_policy_pending: Option<bool>,

    // Mic test monitor (with the capture processor). Frames are 10 ms render
    // frames that played it; dropped frames are underruns of the render side
    // and capture frames that found its ring full.
    pub monitor_active: Option<bool>,
    pub monitor_frames: Option<u64>,
    pub monitor_dropped_frames: Option<u64>,

    // Speech detector (with the DSP chain). The setting native applies; the
    // highest probability since the previous read (-1 when it judged no frame);
    // whether the speech gate judged the last frame and found it speech; and
    // frames with confirmed speech, frames it held the noise gate shut or
    // forced it closed, and frames with detection on but no probability.
    pub speech_detection: Option<SpeechDetection>,
    pub speech_probability: Option<f32>,
    pub speech_detector_running: Option<bool>,
    pub speech: Option<bool>,
    pub speech_frames: Option<u64>,
    pub speech_closed_frames: Option<u64>,
    pub speech_unavailable_frames: Option<u64>,

    // Echo model; `None` without [`ECHO_MODEL_AVAILABLE`].
    pub echo_model: Option<EchoModelStats>,
}

impl Default for RootAudioStats {
    fn default() -> Self {
        Self {
            capture_callbacks: 0,
            render_callbacks: 0,
            capture_max_gap_us: 0,
            render_max_gap_us: 0,
            last_capture_age_us: 0,
            last_render_age_us: 0,
            recording: false,
            playing: false,
            recording_init_result: ADM_RESULT_NOT_ATTEMPTED,
            recording_start_result: ADM_RESULT_NOT_ATTEMPTED,
            playout_init_result: ADM_RESULT_NOT_ATTEMPTED,
            playout_start_result: ADM_RESULT_NOT_ATTEMPTED,
            capture_rate: 0,
            capture_channels: 0,
            capture_raw_sumsq: 0.0,
            capture_raw_samples: 0,
            playout_delay_ms: 0,
            recording_delay_ms: 0,
            mix_calls: 0,
            mix_max_gap_us: 0,
            mix_sources: 0,
            mix_ssrcs: Vec::new(),
            mix_out_sumsq: 0.0,
            mix_out_samples: 0,
            effects_played: 0,
            effects_dropped: 0,
            mix_source_levels: None,
            processed_frames: 0,
            process_rate: 0,
            process_channels: 0,
            apm_out_sumsq: 0.0,
            denoised_sumsq: 0.0,
            gate_in_sumsq: 0.0,
            sent_sumsq: 0.0,
            stage_samples: 0,
            gate_in_block_peak_db: f32::NAN,
            threshold: 0.0,
            envelope: 0.0,
            gate_speaking: false,
            speaking_seq: 0,
            voice_gain_db: f32::NAN,
            voice_gain_settled: false,
            gate_open_frames: 0,
            denoiser_installed: false,
            denoiser_frames: 0,
            denoiser_misses: 0,
            denoiser_empty_outputs: 0,
            denoiser_process_max_us: 0,
            denoiser_slow_frames: 0,
            denoiser_bypass_reason: 0,
            denoiser_notifications: 0,
            erl_db: f64::NAN,
            erle_db: f64::NAN,
            aec_delay_ms: -1,
            divergent_filter_fraction: f64::NAN,
            echo_cancellation_active: false,
            webrtc_ns_active: false,
            audio_thread_lock_violations: 0,
            audio_thread_logs_dropped: 0,
            overlapping_callbacks: 0,
            device_stop_failures: 0,
            speaking: None,
            gate_input_above_frames: None,
            startup_hold_frames: None,
            transmit_frames: None,
            unsupported_frames: None,
            non_finite_samples: None,
            dsp_resets: None,
            dsp_flushes: None,
            gate_open_delay_us: None,
            ns_gap_frames: None,
            denoiser_rate: None,
            denoiser_installs: None,
            denoiser_bypassed_frames: None,
            denoiser_process_us_buckets: Vec::new(),
            denoiser_other_notifications: None,
            denoiser_latency_ms: None,
            denoiser_buffered_samples: None,
            denoiser_underruns: None,
            denoiser_padded_samples: None,
            denoiser_trimmed_samples: None,
            denoiser_drained_samples: None,
            denoiser_frames_until_first_output: None,
            webrtc_ns_fallback: None,
            ns_fallback_pending_frames: None,
            apm_policy_applies: None,
            apm_policy_deferred: None,
            apm_policy_forced: None,
            apm_policy_pending: None,
            monitor_active: None,
            monitor_frames: None,
            monitor_dropped_frames: None,
            speech_detection: None,
            speech_probability: None,
            speech_detector_running: None,
            speech: None,
            speech_frames: None,
            speech_closed_frames: None,
            speech_unavailable_frames: None,
            echo_model: None,
        }
    }
}

impl RootAudioStats {
    pub(crate) fn from_native(native: NativeAudioStats) -> Self {
        let dsp = native.dsp_available;
        let denoiser = native.denoiser_available;
        let policy = native.policy_available;
        let monitor = native.monitor_available;
        let speech = native.speech_available;
        Self {
            speech_detection: speech.then(|| SpeechDetection::from_wire(native.speech_detection)),
            speech_probability: speech.then_some(native.speech_probability),
            speech_detector_running: speech.then_some(native.speech_detector_running),
            speech: speech.then_some(native.speech),
            speech_frames: speech.then_some(native.speech_frames),
            speech_closed_frames: speech.then_some(native.speech_closed_frames),
            speech_unavailable_frames: speech.then_some(native.speech_unavailable_frames),
            monitor_active: monitor.then_some(native.monitor_active),
            monitor_frames: monitor.then_some(native.monitor_frames),
            monitor_dropped_frames: monitor.then_some(native.monitor_dropped_frames),
            denoised_sumsq: native.denoised_sumsq,
            gate_in_sumsq: native.gate_in_sumsq,
            gate_in_block_peak_db: native.gate_in_block_peak_db,
            threshold: native.threshold,
            envelope: native.envelope,
            gate_speaking: native.gate_speaking,
            speaking_seq: native.speaking_seq,
            voice_gain_db: native.voice_gain_db,
            voice_gain_settled: native.voice_gain_settled,
            gate_open_frames: native.gate_open_frames,
            denoiser_installed: native.denoiser_installed,
            denoiser_frames: native.denoiser_frames,
            denoiser_misses: native.denoiser_misses,
            denoiser_empty_outputs: native.denoiser_empty_outputs,
            denoiser_process_max_us: native.denoiser_process_max_us,
            denoiser_slow_frames: native.denoiser_slow_frames,
            denoiser_bypass_reason: native.denoiser_bypass_reason,
            denoiser_notifications: native.denoiser_notifications,
            speaking: dsp.then_some(native.speaking),
            gate_input_above_frames: dsp.then_some(native.gate_input_above_frames),
            startup_hold_frames: dsp.then_some(native.startup_hold_frames),
            transmit_frames: dsp.then_some(native.transmit_frames),
            unsupported_frames: dsp.then_some(native.unsupported_frames),
            non_finite_samples: dsp.then_some(native.non_finite_samples),
            dsp_resets: dsp.then_some(native.dsp_resets),
            dsp_flushes: dsp.then_some(native.dsp_flushes),
            gate_open_delay_us: dsp.then_some(native.gate_open_delay_us),
            ns_gap_frames: dsp.then_some(native.ns_gap_frames),
            denoiser_rate: denoiser.then_some(native.denoiser_rate),
            denoiser_installs: denoiser.then_some(native.denoiser_installs),
            denoiser_bypassed_frames: denoiser.then_some(native.denoiser_bypassed_frames),
            denoiser_process_us_buckets: if denoiser {
                native.denoiser_process_us_buckets
            } else {
                Vec::new()
            },
            denoiser_other_notifications: denoiser.then_some(native.denoiser_other_notifications),
            denoiser_latency_ms: denoiser.then_some(native.denoiser_latency_ms),
            denoiser_buffered_samples: denoiser.then_some(native.denoiser_buffered_samples),
            denoiser_underruns: denoiser.then_some(native.denoiser_underruns),
            denoiser_padded_samples: denoiser.then_some(native.denoiser_padded_samples),
            denoiser_trimmed_samples: denoiser.then_some(native.denoiser_trimmed_samples),
            denoiser_drained_samples: denoiser.then_some(native.denoiser_drained_samples),
            denoiser_frames_until_first_output: denoiser
                .then_some(native.denoiser_frames_until_first_output),
            webrtc_ns_fallback: denoiser.then_some(native.webrtc_ns_fallback),
            ns_fallback_pending_frames: denoiser.then_some(native.ns_fallback_pending_frames),
            apm_policy_applies: policy.then_some(native.apm_policy_applies),
            apm_policy_deferred: policy.then_some(native.apm_policy_deferred),
            apm_policy_forced: policy.then_some(native.apm_policy_forced),
            apm_policy_pending: policy.then_some(native.apm_policy_pending),
            capture_callbacks: native.capture_callbacks,
            render_callbacks: native.render_callbacks,
            capture_max_gap_us: native.capture_max_gap_us,
            render_max_gap_us: native.render_max_gap_us,
            last_capture_age_us: native.last_capture_age_us,
            last_render_age_us: native.last_render_age_us,
            recording: native.recording,
            playing: native.playing,
            recording_init_result: native.recording_init_result,
            recording_start_result: native.recording_start_result,
            playout_init_result: native.playout_init_result,
            playout_start_result: native.playout_start_result,
            capture_rate: native.capture_rate,
            capture_channels: native.capture_channels,
            capture_raw_sumsq: native.capture_raw_sumsq,
            capture_raw_samples: native.capture_raw_samples,
            playout_delay_ms: native.playout_delay_ms,
            recording_delay_ms: native.recording_delay_ms,
            mix_calls: native.mix_calls,
            mix_max_gap_us: native.mix_max_gap_us,
            mix_sources: native.mix_sources,
            mix_ssrcs: native.mix_ssrcs,
            mix_out_sumsq: native.mix_out_sumsq,
            mix_out_samples: native.mix_out_samples,
            effects_played: native.effects_played,
            effects_dropped: native.effects_dropped,
            mix_source_levels: native
                .mix_source_levels_available
                .then(|| native.mix_source_levels.iter().map(MixSourceLevel::from).collect()),
            processed_frames: native.processed_frames,
            process_rate: native.process_rate,
            process_channels: native.process_channels,
            apm_out_sumsq: native.apm_out_sumsq,
            sent_sumsq: native.sent_sumsq,
            stage_samples: native.stage_samples,
            erl_db: native.erl_db,
            erle_db: native.erle_db,
            aec_delay_ms: native.aec_delay_ms,
            divergent_filter_fraction: native.divergent_filter_fraction,
            echo_cancellation_active: native.echo_cancellation_active,
            webrtc_ns_active: native.webrtc_ns_active,
            audio_thread_lock_violations: native.audio_thread_lock_violations,
            audio_thread_logs_dropped: native.audio_thread_logs_dropped,
            overlapping_callbacks: native.overlapping_callbacks,
            device_stop_failures: native.device_stop_failures,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use libwebrtc::peer_connection_factory::native::ADM_RESULT_DEVICE_NOT_FOUND;

    use super::*;

    fn outcome(found: bool, attempted: bool, init: i32, start: i32) -> AudioDeviceOutcome {
        AudioDeviceOutcome {
            found,
            attempted,
            stop_result: ADM_RESULT_NOT_ATTEMPTED,
            init_result: init,
            start_result: start,
        }
    }

    #[test]
    fn device_results_map_each_direction() {
        let results = AudioDeviceResults::from_outcome(
            Some(AudioDeviceRequestOutcome {
                recording: Some(outcome(true, true, 0, 0)),
                playout: Some(outcome(true, false, 1, 1)),
            }),
            (true, true),
        );
        assert_eq!(results.recording, Some(Ok(())));
        assert_eq!(results.playout, Some(Ok(())));
    }

    #[test]
    fn device_results_report_a_missing_device() {
        let results = AudioDeviceResults::from_outcome(
            Some(AudioDeviceRequestOutcome {
                recording: Some(outcome(false, true, ADM_RESULT_DEVICE_NOT_FOUND, 1)),
                playout: Some(outcome(false, false, 1, 1)),
            }),
            (true, true),
        );
        assert_eq!(results.recording, Some(Err(AudioError::DeviceNotFound)));
        assert_eq!(results.playout, Some(Err(AudioError::DeviceNotFound)));
    }

    #[test]
    fn device_results_report_init_and_start_failures() {
        let results = AudioDeviceResults::from_outcome(
            Some(AudioDeviceRequestOutcome {
                recording: Some(outcome(true, true, -1, 1)),
                playout: Some(outcome(true, true, 0, -1)),
            }),
            (true, true),
        );
        assert!(matches!(
            results.recording,
            Some(Err(AudioError::OperationFailed(ref m))) if m.contains("init")
        ));
        assert!(matches!(
            results.playout,
            Some(Err(AudioError::OperationFailed(ref m))) if m.contains("start")
        ));
    }

    #[test]
    fn device_results_report_a_device_that_failed_to_stop() {
        let stuck = AudioDeviceOutcome { stop_result: -1, ..outcome(true, true, 1, 1) };
        let stopped = AudioDeviceOutcome { stop_result: 0, ..outcome(true, true, 0, 0) };
        let results = AudioDeviceResults::from_outcome(
            Some(AudioDeviceRequestOutcome { recording: Some(stopped), playout: Some(stuck) }),
            (true, true),
        );
        assert_eq!(results.recording, Some(Ok(())));
        assert!(matches!(
            results.playout,
            Some(Err(AudioError::OperationFailed(ref m))) if m.contains("stop")
        ));
    }

    #[test]
    fn device_results_leave_unrequested_directions_empty() {
        let results = AudioDeviceResults::from_outcome(
            Some(AudioDeviceRequestOutcome {
                recording: None,
                playout: Some(outcome(true, true, 0, 0)),
            }),
            (false, true),
        );
        assert_eq!(results.recording, None);
        assert_eq!(results.playout, Some(Ok(())));
    }

    #[test]
    fn device_results_fail_requested_directions_when_the_device_thread_is_gone() {
        let results = AudioDeviceResults::from_outcome(None, (true, false));
        assert!(matches!(results.recording, Some(Err(AudioError::OperationFailed(_)))));
        assert_eq!(results.playout, None);
    }

    #[test]
    fn root_audio_stats_fill_phase_1_fields_and_leave_later_stages_unset() {
        let native = NativeAudioStats {
            capture_callbacks: 10,
            render_callbacks: 20,
            capture_max_gap_us: 11_000,
            recording: true,
            playing: true,
            recording_init_result: 0,
            playout_start_result: -1,
            capture_rate: 48_000,
            capture_channels: 2,
            mix_calls: 30,
            mix_sources: 2,
            mix_ssrcs: vec![1, 2],
            effects_played: 7,
            effects_dropped: 8,
            processed_frames: 9,
            process_rate: 16_000,
            process_channels: 1,
            apm_out_sumsq: 1.5,
            sent_sumsq: 0.5,
            stage_samples: 160,
            erl_db: 12.0,
            erle_db: f64::NAN,
            aec_delay_ms: 40,
            divergent_filter_fraction: 0.25,
            echo_cancellation_active: true,
            audio_thread_lock_violations: 3,
            audio_thread_logs_dropped: 4,
            overlapping_callbacks: 5,
            device_stop_failures: 6,
            gate_in_block_peak_db: f32::NAN,
            voice_gain_db: f32::NAN,
            denoiser_frames_until_first_output: -1,
            ..Default::default()
        };
        let stats = RootAudioStats::from_native(native);
        assert_eq!(stats.capture_callbacks, 10);
        assert_eq!(stats.render_callbacks, 20);
        assert_eq!(stats.capture_max_gap_us, 11_000);
        assert!(stats.recording && stats.playing);
        assert_eq!(stats.recording_init_result, 0);
        assert_eq!(stats.playout_start_result, -1);
        assert_eq!(stats.capture_rate, 48_000);
        assert_eq!(stats.mix_ssrcs, vec![1, 2]);
        assert!(stats.echo_cancellation_active && !stats.webrtc_ns_active);
        assert_eq!(
            (
                stats.audio_thread_lock_violations,
                stats.audio_thread_logs_dropped,
                stats.overlapping_callbacks,
                stats.device_stop_failures
            ),
            (3, 4, 5, 6)
        );
        assert_eq!((stats.effects_played, stats.effects_dropped), (7, 8));
        assert_eq!(
            (stats.processed_frames, stats.process_rate, stats.process_channels),
            (9, 16_000, 1)
        );
        assert_eq!((stats.apm_out_sumsq, stats.sent_sumsq, stats.stage_samples), (1.5, 0.5, 160));
        assert_eq!(
            (stats.erl_db, stats.aec_delay_ms, stats.divergent_filter_fraction),
            (12.0, 40, 0.25)
        );
        assert!(stats.erle_db.is_nan());
        assert_eq!((stats.denoised_sumsq, stats.gate_in_sumsq), (0.0, 0.0));
        assert!(!stats.denoiser_installed && !stats.gate_speaking);
        assert!(stats.voice_gain_db.is_nan() && stats.gate_in_block_peak_db.is_nan());
        assert_eq!((stats.speaking, stats.ns_gap_frames, stats.dsp_resets), (None, None, None));
        assert_eq!((stats.denoiser_rate, stats.webrtc_ns_fallback), (None, None));
        assert_eq!(stats.ns_fallback_pending_frames, None);
        assert!(stats.denoiser_process_us_buckets.is_empty());
        assert_eq!((stats.apm_policy_applies, stats.apm_policy_pending), (None, None));
        assert_eq!(stats.mix_source_levels, None);
        assert_eq!(RootAudioStats::default().aec_delay_ms, -1);
    }

    #[test]
    fn root_audio_stats_carry_per_source_levels_when_the_mixer_counts_them() {
        let level = NativeMixSourceLevel { ssrc: 9, source_id: 3, sumsq: 2.5, samples: 960 };
        let native = NativeAudioStats {
            mix_source_levels_available: true,
            mix_source_levels: vec![level],
            ..Default::default()
        };
        let stats = RootAudioStats::from_native(native);
        assert_eq!(
            stats.mix_source_levels,
            Some(vec![MixSourceLevel { ssrc: 9, source_id: 3, sumsq: 2.5, samples: 960 }])
        );

        let idle = NativeAudioStats { mix_source_levels_available: true, ..Default::default() };
        assert_eq!(RootAudioStats::from_native(idle).mix_source_levels, Some(Vec::new()));
    }

    #[test]
    fn root_audio_stats_fill_the_dsp_denoiser_and_policy_fields_they_have() {
        let native = NativeAudioStats {
            dsp_available: true,
            denoiser_available: true,
            policy_available: true,
            denoised_sumsq: 2.0,
            gate_in_sumsq: 3.0,
            gate_in_block_peak_db: -20.0,
            threshold: 0.025,
            envelope: 0.5,
            gate_speaking: true,
            speaking_seq: 4,
            voice_gain_db: 6.0,
            voice_gain_settled: true,
            gate_open_frames: 7,
            speaking: true,
            gate_input_above_frames: 8,
            startup_hold_frames: 20,
            transmit_frames: 9,
            unsupported_frames: 1,
            non_finite_samples: 2,
            dsp_resets: 3,
            dsp_flushes: 4,
            gate_open_delay_us: 1500,
            ns_gap_frames: 5,
            denoiser_installed: true,
            denoiser_frames: 100,
            denoiser_misses: 1,
            denoiser_empty_outputs: 2,
            denoiser_process_max_us: 900,
            denoiser_slow_frames: 1,
            denoiser_bypass_reason: 3,
            denoiser_notifications: 6,
            denoiser_rate: 48_000,
            denoiser_installs: 2,
            denoiser_bypassed_frames: 10,
            denoiser_process_us_buckets: (0..16).collect(),
            denoiser_other_notifications: 1,
            denoiser_latency_ms: 10.5,
            denoiser_buffered_samples: 480,
            denoiser_underruns: 1,
            denoiser_padded_samples: 480,
            denoiser_trimmed_samples: 0,
            denoiser_drained_samples: 960,
            denoiser_frames_until_first_output: 3,
            webrtc_ns_fallback: true,
            ns_fallback_pending_frames: 11,
            apm_policy_applies: 5,
            apm_policy_deferred: 2,
            apm_policy_forced: 1,
            apm_policy_pending: true,
            ..Default::default()
        };
        let stats = RootAudioStats::from_native(native);
        assert_eq!((stats.denoised_sumsq, stats.gate_in_sumsq), (2.0, 3.0));
        assert_eq!(
            (stats.gate_in_block_peak_db, stats.threshold, stats.envelope),
            (-20.0, 0.025, 0.5)
        );
        assert!(stats.gate_speaking && stats.voice_gain_settled);
        assert_eq!((stats.speaking_seq, stats.voice_gain_db, stats.gate_open_frames), (4, 6.0, 7));
        assert_eq!(stats.speaking, Some(true));
        assert_eq!(
            (stats.gate_input_above_frames, stats.startup_hold_frames, stats.transmit_frames),
            (Some(8), Some(20), Some(9))
        );
        assert_eq!((stats.unsupported_frames, stats.non_finite_samples), (Some(1), Some(2)));
        assert_eq!((stats.dsp_resets, stats.dsp_flushes), (Some(3), Some(4)));
        assert_eq!((stats.gate_open_delay_us, stats.ns_gap_frames), (Some(1500), Some(5)));
        assert!(stats.denoiser_installed);
        assert_eq!(
            (stats.denoiser_frames, stats.denoiser_misses, stats.denoiser_empty_outputs),
            (100, 1, 2)
        );
        assert_eq!(
            (
                stats.denoiser_process_max_us,
                stats.denoiser_slow_frames,
                stats.denoiser_bypass_reason
            ),
            (900, 1, 3)
        );
        assert_eq!(stats.denoiser_notifications, 6);
        assert_eq!(
            (stats.denoiser_rate, stats.denoiser_installs, stats.denoiser_bypassed_frames),
            (Some(48_000), Some(2), Some(10))
        );
        assert_eq!(stats.denoiser_process_us_buckets, (0..16).collect::<Vec<u64>>());
        assert_eq!(stats.denoiser_other_notifications, Some(1));
        assert_eq!(
            (stats.denoiser_latency_ms, stats.denoiser_buffered_samples, stats.denoiser_underruns),
            (Some(10.5), Some(480), Some(1))
        );
        assert_eq!(
            (
                stats.denoiser_padded_samples,
                stats.denoiser_trimmed_samples,
                stats.denoiser_drained_samples
            ),
            (Some(480), Some(0), Some(960))
        );
        assert_eq!(stats.denoiser_frames_until_first_output, Some(3));
        assert_eq!(
            (stats.webrtc_ns_fallback, stats.ns_fallback_pending_frames),
            (Some(true), Some(11))
        );
        assert_eq!(
            (stats.apm_policy_applies, stats.apm_policy_deferred, stats.apm_policy_forced),
            (Some(5), Some(2), Some(1))
        );
        assert_eq!(stats.apm_policy_pending, Some(true));
        assert_eq!(
            (stats.monitor_active, stats.monitor_frames, stats.monitor_dropped_frames),
            (None, None, None),
            "a library without the monitor leaves its fields unset"
        );
    }

    #[test]
    fn root_audio_stats_fill_the_monitor_fields_it_has() {
        let native = NativeAudioStats {
            monitor_available: true,
            monitor_active: true,
            monitor_frames: 120,
            monitor_dropped_frames: 3,
            ..Default::default()
        };
        let stats = RootAudioStats::from_native(native);
        assert_eq!(
            (stats.monitor_active, stats.monitor_frames, stats.monitor_dropped_frames),
            (Some(true), Some(120), Some(3))
        );
        let idle = RootAudioStats::from_native(NativeAudioStats {
            monitor_available: true,
            ..Default::default()
        });
        assert_eq!((idle.monitor_active, idle.monitor_frames), (Some(false), Some(0)));
    }

    #[test]
    fn root_audio_stats_fill_the_speech_fields_only_with_the_detector() {
        let native = NativeAudioStats {
            speech_available: true,
            speech_detection: 2,
            speech_probability: 0.75,
            speech_detector_running: true,
            speech: true,
            speech_frames: 40,
            speech_closed_frames: 7,
            speech_unavailable_frames: 3,
            ..Default::default()
        };
        let stats = RootAudioStats::from_native(native);
        assert_eq!(stats.speech_detection, Some(SpeechDetection::Gate));
        assert_eq!(stats.speech_probability, Some(0.75));
        assert_eq!((stats.speech_detector_running, stats.speech), (Some(true), Some(true)));
        assert_eq!(
            (stats.speech_frames, stats.speech_closed_frames, stats.speech_unavailable_frames),
            (Some(40), Some(7), Some(3))
        );
        let without = RootAudioStats::from_native(NativeAudioStats {
            speech_detection: 2,
            speech_frames: 40,
            ..Default::default()
        });
        assert_eq!((without.speech_detection, without.speech_frames), (None, None));
        assert_eq!((without.speech_probability, without.speech), (None, None));
    }

    #[test]
    fn speech_detection_maps_unknown_wire_values_to_off() {
        for mode in [SpeechDetection::Off, SpeechDetection::Ring, SpeechDetection::Gate] {
            assert_eq!(SpeechDetection::from_wire(mode.to_wire()), mode);
        }
        assert_eq!(SpeechDetection::from_wire(3), SpeechDetection::Off);
        let native = RootAudioControl {
            speech_detection: Some(SpeechDetection::Ring),
            ..Default::default()
        }
        .to_native();
        assert!(native.has_speech_detection && native.speech_detection == 1);
        assert!(!RootAudioControl::default().to_native().has_speech_detection);
    }

    #[test]
    fn echo_model_wire_values_round_trip_and_unknown_values_are_off() {
        for mode in [EchoModel::Off, EchoModel::On, EchoModel::Auto] {
            assert_eq!(EchoModel::from_wire(mode.to_wire()), mode);
        }
        assert_eq!(EchoModel::from_wire(3), EchoModel::Off);
        assert_eq!(EchoModel::from_wire(u32::MAX), EchoModel::Off);
        for state in [
            EchoModelState::Off,
            EchoModelState::On,
            EchoModelState::Starting,
            EchoModelState::Stopping,
        ] {
            assert_eq!(EchoModelState::from_wire(state.to_wire()), state);
        }
        for reason in [
            EchoModelReason::ModeOff,
            EchoModelReason::ModeOn,
            EchoModelReason::DeviceKind,
            EchoModelReason::MeasuredEcho,
            EchoModelReason::KillSwitch,
            EchoModelReason::Unavailable,
        ] {
            assert_eq!(EchoModelReason::from_wire(reason.to_wire()), reason);
        }
        let kinds = [
            OutputDeviceKind::NoDevice,
            OutputDeviceKind::Unknown,
            OutputDeviceKind::Speakers,
            OutputDeviceKind::LineOut,
            OutputDeviceKind::DisplayAudio,
            OutputDeviceKind::Headphones,
            OutputDeviceKind::Headset,
            OutputDeviceKind::Handset,
            OutputDeviceKind::Bluetooth,
        ];
        for (wire, kind) in kinds.into_iter().enumerate() {
            assert_eq!(kind.to_wire(), wire as u32);
            assert_eq!(OutputDeviceKind::from_wire(wire as u32), kind);
        }
        assert_eq!(OutputDeviceKind::from_wire(9), OutputDeviceKind::Unknown);
        let loudspeakers: Vec<_> = kinds.into_iter().filter(|k| k.is_loudspeaker()).collect();
        assert_eq!(
            loudspeakers,
            [OutputDeviceKind::Speakers, OutputDeviceKind::LineOut, OutputDeviceKind::DisplayAudio]
        );
    }

    #[test]
    fn echo_model_control_is_supported_exactly_where_the_native_model_is() {
        assert!(RootAudioControl::default().supported_by_this_build());
        assert!(RootAudioControl { mic_muted: Some(true), ..Default::default() }
            .supported_by_this_build());
        for mode in [EchoModel::Off, EchoModel::On, EchoModel::Auto] {
            let control = RootAudioControl { echo_model: Some(mode), ..Default::default() };
            assert_eq!(control.supported_by_this_build(), ECHO_MODEL_AVAILABLE);
            assert_eq!(control.to_native(), RootAudioControl::default().to_native());
        }
        assert_eq!(RootAudioStats::from_native(NativeAudioStats::default()).echo_model, None);
    }

    #[test]
    fn monitor_results_map_to_errors_with_stable_messages() {
        assert_eq!(monitor_result(MonitorResult::Applied), Ok(()));
        assert_eq!(monitor_result(MonitorResult::Sending), Err(MonitorError::Sending));
        assert_eq!(monitor_result(MonitorResult::NotAcquired), Err(MonitorError::NotAcquired));
        assert_eq!(
            monitor_result(MonitorResult::NoDevice).unwrap_err().to_string(),
            "platform audio has no device module"
        );
        assert_eq!(
            monitor_result(MonitorResult::NotSupported).unwrap_err().to_string(),
            "not supported by this build"
        );
        assert!(MonitorError::Sending.to_string().contains("sending"));
    }

    #[test]
    fn root_audio_denoiser_results_map_empty_errors_to_success() {
        let ok: DenoiserCompletion =
            NativeDenoiserCompletion { owner: 1, async_id: 2, error: String::new(), pipeline: 9 }
                .into();
        assert_eq!(ok, DenoiserCompletion { owner: 1, async_id: 2, error: None, pipeline: 0 });
        let failed: DenoiserCompletion = NativeDenoiserCompletion {
            owner: 1,
            async_id: 3,
            error: "superseded".into(),
            pipeline: 0x1000,
        }
        .into();
        assert_eq!(failed.error.as_deref(), Some("superseded"));
        assert_eq!(failed.pipeline, 0x1000);

        let all: ReturnedPipelines =
            NativeReturnPipelines { pipelines: vec![5, 6], error: String::new() }.into();
        assert_eq!(all, ReturnedPipelines { pipelines: vec![5, 6], error: None });
        let busy: ReturnedPipelines =
            NativeReturnPipelines { pipelines: vec![], error: "busy".into() }.into();
        assert_eq!(busy.error.as_deref(), Some("busy"));
    }

    #[test]
    fn root_audio_control_sanitizes_every_value() {
        let control = RootAudioControl {
            input_volume: Some(f32::NAN),
            master_volume: Some(f32::INFINITY),
            denoise_power: Some(-0.5),
            gate_threshold: Some(f32::NAN),
            gain_reset_db: Some(f32::NEG_INFINITY),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(control.input_volume, Some(0.0));
        assert_eq!(control.master_volume, Some(2.0));
        assert_eq!(control.denoise_power, Some(0.0));
        assert_eq!(control.gate_threshold, Some(0.0));
        assert_eq!(control.gain_reset_db, None);

        let control = RootAudioControl {
            input_volume: Some(1.25),
            master_volume: Some(-3.0),
            denoise_power: Some(7.0),
            gate_threshold: Some(0.05),
            gain_reset_db: Some(40.0),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(control.input_volume, Some(1.25));
        assert_eq!(control.master_volume, Some(0.0));
        assert_eq!(control.denoise_power, Some(1.0));
        assert_eq!(control.gate_threshold, Some(0.05));
        assert_eq!(control.gain_reset_db, Some(MAX_VOICE_GAIN_DB));
    }

    #[test]
    fn root_audio_control_marks_only_the_fields_it_sets() {
        let native = RootAudioControl {
            mic_muted: Some(true),
            push_to_talk: Some(PushToTalk::Held),
            master_volume: Some(f32::NAN),
            webrtc_noise_suppression: Some(false),
            denoiser_wanted: Some(true),
            ..Default::default()
        }
        .to_native();
        assert!(native.has_mic_muted && native.mic_muted);
        assert!(native.has_push_to_talk && native.push_to_talk == 1);
        assert!(native.has_master_volume && native.master_volume == 0.0);
        assert!(native.has_noise_suppression && !native.noise_suppression);
        assert!(native.has_denoiser_wanted && native.denoiser_wanted);
        assert!(!native.has_input_volume && !native.has_deafened && !native.has_echo_cancellation);
        assert!(!native.has_denoise_power && !native.has_auto_gain && !native.has_gate_threshold);
        assert!(!native.has_auto_sensitivity && !native.has_gain_reset_db);
        assert_eq!(RootAudioControl::default().to_native(), Default::default());
    }

    #[test]
    fn push_to_talk_closes_the_gate_on_unknown_values() {
        assert_eq!(PushToTalk::from_wire(0), PushToTalk::VoiceActivity);
        assert_eq!(PushToTalk::from_wire(1), PushToTalk::Held);
        assert_eq!(PushToTalk::from_wire(2), PushToTalk::Released);
        assert_eq!(PushToTalk::from_wire(7), PushToTalk::Released);
        for state in [PushToTalk::VoiceActivity, PushToTalk::Held, PushToTalk::Released] {
            assert_eq!(PushToTalk::from_wire(state.to_wire()), state);
        }
    }

    #[test]
    fn sanitize_volume_matches_the_apps_clamp() {
        assert_eq!(sanitize_volume(f32::NAN), 0.0);
        assert_eq!(sanitize_volume(7.0), 2.0);
        assert_eq!(sanitize_volume(-1.0), 0.0);
        assert_eq!(sanitize_volume(0.5), 0.5);
    }
}
