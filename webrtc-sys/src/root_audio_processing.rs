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

//! The capture post-processor's voice chain (`rootapp-voice-dsp`, called from
//! `RootCaptureProcessor`), and test hooks for the factory's APM: the
//! post-processor and the decorator (`RootApmDecorator`), which are otherwise
//! only reachable through libwebrtc.

use rootapp_voice_dsp::{
    sanitize, Agc2Tuning, DenoiserOutput, DspChain, DspCommands, DspOutputs, DspSettings,
    GainEngine, PushToTalk, SpeechDetection, VoiceGainTuning,
};

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    /// `rootapp_voice_dsp::DspSettings`; push_to_talk 0 voice activity, 1 held,
    /// anything else released; speech_detection 0 off, 1 ring, 2 gate, anything
    /// else off; speech_probability the detector's for this frame (negative:
    /// it did not run); capture_silent the decorator found this frame's raw
    /// input digitally silent; auto_sensitivity_minimum_db and voice_gain as
    /// the host set them (NaN: the chain's default); gain_engine 0 the chain's
    /// voice gain, 1 AGC2 between post_denoise_align and post_gain (anything
    /// else 0), with the AGC2 that ran in agc2.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootDspSettings {
        pub sample_rate: u32,
        pub mic_muted: bool,
        pub push_to_talk: u32,
        pub input_volume: f32,
        pub auto_gain: bool,
        pub gate_threshold: f32,
        pub auto_sensitivity: bool,
        pub denoiser_wanted: bool,
        pub speech_detection: u32,
        pub speech_probability: f32,
        pub capture_silent: bool,
        pub auto_sensitivity_minimum_db: f32,
        pub voice_gain: RootVoiceGainTuning,
        pub gain_engine: u32,
        pub agc2: RootAgc2Tuning,
    }

    /// `rootapp_voice_dsp::Agc2Tuning`: AGC2's adaptive digital settings (dB,
    /// dB, dB/s, dBFS; NaN: the default).
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct RootAgc2Tuning {
        pub headroom_db: f32,
        pub max_gain_db: f32,
        pub max_gain_change_db_per_second: f32,
        pub max_output_noise_dbfs: f32,
    }

    /// `rootapp_voice_dsp::Aligned`.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootDspAligned {
        pub supported: bool,
        pub pre_gain_rms: f32,
    }

    /// `rootapp_voice_dsp::VoiceGainTuning`: the voice gain's tunables (dB, dBFS,
    /// dB/s, ms, probability; NaN: the chain's default).
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct RootVoiceGainTuning {
        pub max_gain_db: f32,
        pub target_db: f32,
        pub rise_db_per_second: f32,
        pub fall_db_per_second: f32,
        pub level_time_constant_ms: f32,
        pub min_speech_probability: f32,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootDspCommands {
        pub reset: bool,
        pub flush: bool,
        pub gain_reset: bool,
        pub gain_reset_db: f32,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootDspPre {
        pub flush_denoiser: bool,
        pub supported: bool,
    }

    /// `rootapp_voice_dsp::DspOutputs` with the aligner stats flattened.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootDspOutputs {
        pub samples: u32,
        pub apm_out_sumsq: f64,
        pub denoised_sumsq: f64,
        pub gate_in_sumsq: f64,
        pub sent_sumsq: f64,
        pub gate_in_rms: f32,
        pub gate_in_block_peak_db: f32,
        pub threshold: f32,
        pub envelope: f32,
        pub gate_speaking: bool,
        pub speaking: bool,
        pub gate_open: bool,
        pub gate_input_above: bool,
        pub held: bool,
        pub transmit_open: bool,
        pub transmitting: bool,
        pub voice_gain_db: f32,
        pub voice_gain_settled: bool,
        pub denoiser_ran: bool,
        pub denoiser_stalled: bool,
        pub aligner_primed: bool,
        pub aligner_buffered_samples: u32,
        pub aligner_underruns: u64,
        pub aligner_padded_samples: u64,
        pub aligner_trimmed_samples: u64,
        pub aligner_drained_samples: u64,
        pub aligner_latency_ms: f32,
        pub unsupported: bool,
        pub non_finite_samples: u32,
        pub reset: bool,
        pub flushed: bool,
        pub speech_probability: f32,
        pub speech: bool,
        pub speech_closed: bool,
        /// The engine that boosted the frame (0 the chain's, 1 AGC2), and the
        /// chain's voice gain's view: the room before the denoiser and the
        /// talker's level (NaN until measured), the floor after it, and what
        /// held the gain (0 none yet, 1 target, 2 max, 3 noise, 4 room).
        pub gain_engine: u32,
        pub voice_gain_room_db: f32,
        pub voice_gain_speech_level_db: f32,
        pub voice_gain_floor_db: f32,
        pub voice_gain_limit: u32,
    }

    extern "Rust" {
        type RootDspChain;

        /// The auto sensitivity minimum (dBFS before the voice gain) the chain
        /// applies for a host's setting: non-finite is the default (-54), the
        /// rest clamped to -80..-30.
        fn root_auto_sensitivity_minimum_db(requested: f32) -> f32;
        /// The voice gain's tunables the chain applies for a host's settings:
        /// each clamped to its range, a non-finite one the default.
        fn root_voice_gain_tuning(requested: RootVoiceGainTuning) -> RootVoiceGainTuning;
        /// AGC2's settings for a host's: each clamped to its range, a
        /// non-finite one the default.
        fn root_agc2_tuning(requested: RootAgc2Tuning) -> RootAgc2Tuning;
        fn new_root_dsp_chain(max_rate: u32) -> Box<RootDspChain>;
        fn pre_denoise(
            self: &mut RootDspChain,
            frame: &mut [f32],
            settings: &RootDspSettings,
            commands: &RootDspCommands,
            outputs: &mut RootDspOutputs,
        ) -> RootDspPre;
        /// kind: 0 not in use, 1 missed, 2 ran (denoised and restart are used
        /// only for 2). Leaves the aligned voice before any gain in `frame`.
        fn post_denoise_align(
            self: &mut RootDspChain,
            frame: &mut [f32],
            kind: u8,
            denoised: &[f32],
            restart: bool,
            settings: &RootDspSettings,
            outputs: &mut RootDspOutputs,
        ) -> RootDspAligned;
        /// gain_db: the gain AGC2 applied to `frame` (gain_engine 1).
        fn post_gain(
            self: &mut RootDspChain,
            frame: &mut [f32],
            pre_gain_rms: f32,
            gain_db: f32,
            settings: &RootDspSettings,
            outputs: &mut RootDspOutputs,
        );
    }

    #[derive(Debug, Clone, Default)]
    pub struct RootProcessorFrame {
        pub samples: Vec<f32>,
        /// Every other channel equals channel 0.
        pub copies_match: bool,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootProcessorStats {
        pub format_seq: u64,
        pub process_rate: u32,
        pub process_channels: u32,
        pub processed_frames: u64,
        pub apm_out_sumsq: f64,
        pub sent_sumsq: f64,
        pub stage_samples: u64,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootDecoratorProbe {
        /// Inner ApplyConfig calls the decorator made.
        pub inner_applies: u64,
        /// ApplyConfig calls the fake APM got.
        pub fake_applies: u64,
        pub inner_aec: bool,
        pub inner_ns: bool,
        pub inner_agc1: bool,
        pub inner_agc2: bool,
        pub inner_hpf: bool,
        pub fake_mute_calls: u64,
        pub fake_output_used_settings: u64,
        pub output_will_be_muted: bool,
        pub swallowed: u64,
        pub aec_active: bool,
        pub ns_active: bool,
        /// What GetConfig serves the voice engine.
        pub served_aec: bool,
        pub served_ns: bool,
        pub served_agc: bool,
        pub served_hpf: bool,
    }

    /// `RootCaptureState::ReadDspStats` and `ReadSpeaking`.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootProcessorDsp {
        pub processed_frames: u64,
        pub stage_samples: u64,
        pub apm_out_sumsq: f64,
        pub denoised_sumsq: f64,
        pub gate_in_sumsq: f64,
        pub sent_sumsq: f64,
        pub gate_open_frames: u64,
        pub gate_input_above_frames: u64,
        pub startup_hold_frames: u64,
        pub transmit_frames: u64,
        pub unsupported_frames: u64,
        pub non_finite_samples: u64,
        pub dsp_resets: u64,
        pub dsp_flushes: u64,
        pub ns_gap_frames: u64,
        pub ns_fallback_pending_frames: u64,
        pub speaking_seq: u64,
        pub gate_open_delay_us: u64,
        pub threshold: f32,
        pub voice_gain_db: f32,
        pub voice_gain_settled: bool,
        pub gate_speaking: bool,
        pub speaking: bool,
        pub gate_in_block_peak_db: f32,
        pub envelope_max: f32,
        pub aligner_primed: bool,
        pub aligner_buffered_samples: u32,
        pub aligner_underruns: u64,
        pub aligner_padded_samples: u64,
        pub aligner_trimmed_samples: u64,
        pub aligner_drained_samples: u64,
        pub aligner_latency_ms: f32,
        pub published_speaking: bool,
        pub published_seq: u64,
        pub speech_detection: u32,
        /// NaN: the detector judged no frame since the last take.
        pub speech_probability_max: f32,
        pub speech_detector_running: bool,
        pub speech: bool,
        pub speech_frames: u64,
        pub speech_closed_frames: u64,
        pub speech_unavailable_frames: u64,
        /// The stored setting (NaN: none).
        pub auto_sensitivity_minimum_db: f32,
        /// The stored settings (NaN: none).
        pub voice_gain: RootVoiceGainTuning,
        /// The stored gain engine (0 the chain's, 1 AGC2) and the one that ran
        /// the last frame.
        pub gain_engine: u32,
        pub gain_engine_applied: u32,
        pub voice_gain_room_db: f32,
        pub voice_gain_speech_level_db: f32,
        pub voice_gain_floor_db: f32,
        pub voice_gain_limit: u32,
        /// The stored AGC2 settings (NaN: none), and the AGC2 built last: its
        /// settings, its rate (0 none) and how many were built.
        pub agc2: RootAgc2Tuning,
        pub agc2_applied: RootAgc2Tuning,
        pub agc2_rate: u32,
        pub agc2_builds: u64,
    }

    /// RootSpeechDetector over a synthetic signal (`root_speech_detector_probe`).
    /// The statistics skip the first 200 ms (the detector's warm-up) and
    /// frames it could not analyze.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootSpeechDetectorProbe {
        pub frames: u32,
        /// Frames the detector returned -1 for (a rate it does not run at).
        pub unsupported: u32,
        /// Largest difference from libwebrtc's VoiceActivityDetectorWrapper
        /// on the same frames, until the detector's first latch reset (0 at
        /// rates the wrapper is not compared at).
        pub max_diff: f32,
        pub judged: u32,
        pub mean: f32,
        pub max: f32,
        /// Frames at 0.5 or above, and the longest run of them.
        pub above: u32,
        pub longest_run: u32,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootFakeDenoiserProbe {
        pub process_calls: u64,
        pub flush_calls: u64,
        pub power_sets: u64,
        pub last_power: f32,
        pub installed: bool,
        /// RootDenoiserBypass.
        pub bypass: u32,
        /// Frames the slot was busy for.
        pub misses: u64,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootPolicyProbe {
        pub applies: u64,
        pub deferred: u64,
        pub forced: u64,
        pub pending: bool,
        pub fallback_ns: bool,
        /// What the decorator stored in the capture state.
        pub webrtc_ns_applied: bool,
    }

    unsafe extern "C++" {
        include!("livekit/root_audio_processing.h");

        type RootProcessorHarness;
        type RootApmHarness;

        /// The factory builds the voice chain (desktop): the DSP capability.
        fn root_dsp_supported() -> bool;

        fn new_root_processor_harness() -> UniquePtr<RootProcessorHarness>;
        /// Without a denoiser slot (32-bit and mobile builds).
        fn new_root_processor_harness_without_slot() -> UniquePtr<RootProcessorHarness>;
        fn initialize(self: &RootProcessorHarness, rate: i32, channels: i32);
        fn set_mic_muted(self: &RootProcessorHarness, muted: bool);
        fn set_push_to_talk(self: &RootProcessorHarness, state: u32);
        /// Both gate fields in one update; -1 keeps a field.
        fn set_gate(self: &RootProcessorHarness, mic_muted: i8, push_to_talk: i32);
        fn set_input_volume(self: &RootProcessorHarness, volume: f32);
        fn reset_controls(self: &RootProcessorHarness);
        fn set_dsp(
            self: &RootProcessorHarness,
            auto_gain: bool,
            auto_sensitivity: bool,
            gate_threshold: f32,
        );
        fn set_denoiser_wanted(self: &RootProcessorHarness, wanted: bool);
        fn set_denoise_power(self: &RootProcessorHarness, power: f32);
        /// 0 off, 1 ring, 2 gate (as the controller stores it).
        fn set_speech_detection(self: &RootProcessorHarness, mode: u32);
        fn set_auto_sensitivity_minimum_db(self: &RootProcessorHarness, db: f32);
        /// As the controller stores them (NaN: none).
        fn set_voice_gain_tuning(self: &RootProcessorHarness, tuning: RootVoiceGainTuning);
        /// As the APM decorator does before the next frame.
        fn set_capture_silent(self: &RootProcessorHarness, silent: bool);
        fn set_webrtc_ns_applied(self: &RootProcessorHarness, applied: bool);
        /// As the denoiser control does when it decides the NS fallback.
        fn set_ns_fallback_decided(self: &RootProcessorHarness, on: bool);
        /// As AdmProxy does before each StartRecording.
        fn bump_stream_epoch(self: &RootProcessorHarness);
        /// As the controller does: payload, then the sequence number (and AGC2
        /// starts over from it).
        fn gain_reset(self: &RootProcessorHarness, db: f32);
        /// 0 the chain's voice gain, 1 AGC2, as the controller stores it (and
        /// builds what AGC2 needs at once, as it does).
        fn set_gain_engine(self: &RootProcessorHarness, engine: u32);
        fn set_agc2_tuning(self: &RootProcessorHarness, tuning: RootAgc2Tuning);
        /// The control side's 10 ms poll: AGC2 for the processor's rate.
        fn service_agc2(self: &RootProcessorHarness);
        fn process(
            self: &RootProcessorHarness,
            rate: i32,
            channels: usize,
            input: &[f32],
        ) -> RootProcessorFrame;
        fn stats(self: &RootProcessorHarness) -> RootProcessorStats;
        fn dsp(self: &RootProcessorHarness, take_maxima: bool) -> RootProcessorDsp;
        /// A RootFakeFxPipeline (output = round(scale x input) delayed by
        /// delay_samples) installed in the processor's denoiser slot.
        fn install_fake_denoiser(
            self: &RootProcessorHarness,
            rate: u32,
            scale: f32,
            delay_samples: u32,
            burst_call: u32,
            empty_from: u32,
            empty_count: u32,
        ) -> bool;
        fn remove_fake_denoiser(self: &RootProcessorHarness) -> bool;
        fn fake_denoiser(self: &RootProcessorHarness) -> RootFakeDenoiserProbe;
        /// Holds the slot as the audio thread would mid-process.
        fn hold_denoiser_slot(self: &RootProcessorHarness, hold: bool) -> bool;
        /// Stores the speaking flag as the processor publishes it, as of a
        /// frame now.
        fn set_published_speaking(self: &RootProcessorHarness, speaking: bool);
        /// The control side's check for frames that stopped with speaking set.
        fn clear_stale_speaking(self: &RootProcessorHarness);
        fn gate_open_stamp_us(self: &RootProcessorHarness) -> i64;
        /// Speaking and outside the startup hold.
        fn audibly_speaking(self: &RootProcessorHarness) -> bool;

        /// The decorator over a recording fake APM, or over the real builtin APM
        /// with the capture processor.
        fn new_root_apm_harness(real_apm: bool) -> UniquePtr<RootApmHarness>;
        fn engine_apply(
            self: &RootApmHarness,
            echo_cancellation: bool,
            noise_suppression: bool,
            auto_gain_control: bool,
            gain_controller2: bool,
            high_pass_filter: bool,
        );
        /// GetConfig, change the echo canceller only, ApplyConfig.
        fn engine_apply_echo_cancellation(self: &RootApmHarness, echo_cancellation: bool);
        fn set_output_will_be_muted(self: &RootApmHarness, muted: bool);
        fn post_capture_output_used(self: &RootApmHarness, used: bool) -> bool;
        /// -1 leaves that part of the policy unchanged, 0 off, 1 on.
        fn set_policy(self: &RootApmHarness, echo_cancellation: i8, noise_suppression: i8);
        fn reset_policy(self: &RootApmHarness);
        fn advance_clock(self: &RootApmHarness, ms: i64);
        fn run_due_tasks(self: &RootApmHarness) -> u32;
        fn pending_tasks(self: &RootApmHarness) -> u32;
        fn probe(self: &RootApmHarness) -> RootDecoratorProbe;
        fn process_frame(self: &RootApmHarness, rate: i32, channels: usize, amplitude: i16) -> i16;
        /// The same through the float ProcessStream (a ±1 signal).
        fn process_float_frame(
            self: &RootApmHarness,
            rate: i32,
            channels: usize,
            amplitude: f32,
        ) -> f32;
        fn set_mic_muted(self: &RootApmHarness, muted: bool);
        fn set_dsp(
            self: &RootApmHarness,
            auto_gain: bool,
            auto_sensitivity: bool,
            gate_threshold: f32,
        );
        fn set_speech_detection(self: &RootApmHarness, mode: u32);
        fn dsp(self: &RootApmHarness, take_maxima: bool) -> RootProcessorDsp;
        fn processed_frames(self: &RootApmHarness) -> u64;
        fn processor_rate(self: &RootApmHarness) -> u32;
        fn processor_channels(self: &RootApmHarness) -> u32;
        /// The capture state's published speaking flag.
        fn speaking(self: &RootApmHarness) -> bool;
        /// Overrides the published speaking flag the decorator waits on, as of
        /// a frame now.
        fn set_speaking(self: &RootApmHarness, speaking: bool);
        fn set_fallback_ns(self: &RootApmHarness, on: bool);
        fn policy(self: &RootApmHarness) -> RootPolicyProbe;

        /// The engine's ApplyConfig racing policy changes on another thread.
        fn root_apm_decorator_race_for_test(iterations: u32) -> bool;
        /// A policy stats read during another thread's inner ApplyConfig
        /// returns at once.
        fn root_apm_policy_stats_never_wait_for_an_apply_for_test() -> bool;

        /// `frames` 10 ms frames of a synthetic FloatS16 signal: 0 vowel, 1 white
        /// noise, 2 clicks, 3 tone, 4 silence, 5 breath, 6 keys.
        fn root_speech_test_signal(signal: u32, rate: i32, frames: u32) -> Vec<f32>;
        /// RootSpeechDetector over that signal, next to libwebrtc's wrapper.
        fn root_speech_detector_probe(
            signal: u32,
            rate: i32,
            frames: u32,
        ) -> RootSpeechDetectorProbe;
        /// A fresh detector's probability for each whole 10 ms frame of `input`.
        fn root_speech_detector_probabilities(rate: i32, input: &[f32]) -> Vec<f32>;
        /// The detector over `frames` frames of white noise at -60 dBFS.
        fn root_speech_detector_hiss(rate: i32, frames: u32) -> RootSpeechDetectorProbe;
        /// A 16 kHz vowel, a 48 kHz vowel, then 16 kHz silence: its first
        /// frame's probability.
        fn root_speech_detector_rate_switch_for_test() -> f32;
    }
}

/// The voice chain the capture processor owns. The chain's own outputs are
/// kept here between calls, so an unsupported frame reports the last values.
pub struct RootDspChain {
    chain: DspChain,
    out: DspOutputs,
}

fn new_root_dsp_chain(max_rate: u32) -> Box<RootDspChain> {
    Box::new(RootDspChain { chain: DspChain::new(max_rate), out: DspOutputs::default() })
}

fn root_auto_sensitivity_minimum_db(requested: f32) -> f32 {
    sanitize::auto_sensitivity_minimum_db(requested)
}

fn voice_gain_tuning(t: &ffi::RootVoiceGainTuning) -> VoiceGainTuning {
    VoiceGainTuning {
        max_gain_db: t.max_gain_db,
        target_db: t.target_db,
        rise_db_per_second: t.rise_db_per_second,
        fall_db_per_second: t.fall_db_per_second,
        level_time_constant_ms: t.level_time_constant_ms,
        min_speech_probability: t.min_speech_probability,
    }
}

fn root_voice_gain_tuning(requested: ffi::RootVoiceGainTuning) -> ffi::RootVoiceGainTuning {
    let t = voice_gain_tuning(&requested).sanitized();
    ffi::RootVoiceGainTuning {
        max_gain_db: t.max_gain_db,
        target_db: t.target_db,
        rise_db_per_second: t.rise_db_per_second,
        fall_db_per_second: t.fall_db_per_second,
        level_time_constant_ms: t.level_time_constant_ms,
        min_speech_probability: t.min_speech_probability,
    }
}

fn agc2_tuning(t: &ffi::RootAgc2Tuning) -> Agc2Tuning {
    Agc2Tuning {
        headroom_db: t.headroom_db,
        max_gain_db: t.max_gain_db,
        max_gain_change_db_per_second: t.max_gain_change_db_per_second,
        max_output_noise_dbfs: t.max_output_noise_dbfs,
    }
}

fn root_agc2_tuning(requested: ffi::RootAgc2Tuning) -> ffi::RootAgc2Tuning {
    let t = agc2_tuning(&requested).sanitized();
    ffi::RootAgc2Tuning {
        headroom_db: t.headroom_db,
        max_gain_db: t.max_gain_db,
        max_gain_change_db_per_second: t.max_gain_change_db_per_second,
        max_output_noise_dbfs: t.max_output_noise_dbfs,
    }
}

fn dsp_settings(settings: &ffi::RootDspSettings) -> DspSettings {
    DspSettings {
        sample_rate: settings.sample_rate,
        mic_muted: settings.mic_muted,
        push_to_talk: PushToTalk::from_wire(settings.push_to_talk),
        input_volume: settings.input_volume,
        auto_gain: settings.auto_gain,
        gate_threshold: settings.gate_threshold,
        auto_sensitivity: settings.auto_sensitivity,
        denoiser_wanted: settings.denoiser_wanted,
        speech_detection: SpeechDetection::from_wire(settings.speech_detection),
        speech_probability: settings.speech_probability,
        capture_silent: settings.capture_silent,
        auto_sensitivity_minimum_db: settings.auto_sensitivity_minimum_db,
        voice_gain: voice_gain_tuning(&settings.voice_gain),
        gain_engine: GainEngine::from_wire(settings.gain_engine),
        agc2: agc2_tuning(&settings.agc2),
    }
}

fn copy_outputs(from: &DspOutputs, to: &mut ffi::RootDspOutputs) {
    *to = ffi::RootDspOutputs {
        samples: from.samples,
        apm_out_sumsq: from.apm_out_sumsq,
        denoised_sumsq: from.denoised_sumsq,
        gate_in_sumsq: from.gate_in_sumsq,
        sent_sumsq: from.sent_sumsq,
        gate_in_rms: from.gate_in_rms,
        gate_in_block_peak_db: from.gate_in_block_peak_db,
        threshold: from.threshold,
        envelope: from.envelope,
        gate_speaking: from.gate_speaking,
        speaking: from.speaking,
        gate_open: from.gate_open,
        gate_input_above: from.gate_input_above,
        held: from.held,
        transmit_open: from.transmit_open,
        transmitting: from.transmitting,
        voice_gain_db: from.voice_gain_db,
        voice_gain_settled: from.voice_gain_settled,
        denoiser_ran: from.denoiser_ran,
        denoiser_stalled: from.denoiser_stalled,
        aligner_primed: from.aligner.primed,
        aligner_buffered_samples: from.aligner.buffered_samples,
        aligner_underruns: from.aligner.underruns,
        aligner_padded_samples: from.aligner.padded_samples,
        aligner_trimmed_samples: from.aligner.trimmed_samples,
        aligner_drained_samples: from.aligner.drained_samples,
        aligner_latency_ms: from.aligner.latency_ms,
        unsupported: from.unsupported,
        non_finite_samples: from.non_finite_samples,
        reset: from.reset,
        flushed: from.flushed,
        speech_probability: from.speech_probability,
        speech: from.speech,
        speech_closed: from.speech_closed,
        gain_engine: from.gain_engine.to_wire(),
        voice_gain_room_db: from.voice_gain_room_db,
        voice_gain_speech_level_db: from.voice_gain_speech_level_db,
        voice_gain_floor_db: from.voice_gain_floor_db,
        voice_gain_limit: from.voice_gain_limit.to_wire(),
    };
}

impl RootDspChain {
    fn pre_denoise(
        &mut self,
        frame: &mut [f32],
        settings: &ffi::RootDspSettings,
        commands: &ffi::RootDspCommands,
        outputs: &mut ffi::RootDspOutputs,
    ) -> ffi::RootDspPre {
        let commands = DspCommands {
            reset: commands.reset,
            flush: commands.flush,
            gain_reset: commands.gain_reset,
            gain_reset_db: commands.gain_reset_db,
        };
        let pre = self.chain.pre_denoise(frame, &dsp_settings(settings), &commands, &mut self.out);
        copy_outputs(&self.out, outputs);
        ffi::RootDspPre { flush_denoiser: pre.flush_denoiser, supported: pre.supported }
    }

    fn post_denoise_align(
        &mut self,
        frame: &mut [f32],
        kind: u8,
        denoised: &[f32],
        restart: bool,
        settings: &ffi::RootDspSettings,
        outputs: &mut ffi::RootDspOutputs,
    ) -> ffi::RootDspAligned {
        let denoised = match kind {
            1 => DenoiserOutput::Missed,
            2 => DenoiserOutput::Ran { samples: denoised, restart },
            _ => DenoiserOutput::NotInUse,
        };
        let aligned =
            self.chain.post_denoise_align(frame, denoised, &dsp_settings(settings), &mut self.out);
        copy_outputs(&self.out, outputs);
        ffi::RootDspAligned { supported: aligned.supported, pre_gain_rms: aligned.pre_gain_rms }
    }

    fn post_gain(
        &mut self,
        frame: &mut [f32],
        pre_gain_rms: f32,
        gain_db: f32,
        settings: &ffi::RootDspSettings,
        outputs: &mut ffi::RootDspOutputs,
    ) {
        self.chain.post_gain(frame, pre_gain_rms, gain_db, &dsp_settings(settings), &mut self.out);
        copy_outputs(&self.out, outputs);
    }
}

#[cfg(test)]
mod processor_tests {
    use super::ffi::{
        new_root_processor_harness, new_root_processor_harness_without_slot, RootProcessorHarness,
        RootVoiceGainTuning,
    };
    use super::root_voice_gain_tuning;

    const VOICE_ACTIVITY: u32 = 0;
    const HELD: u32 = 1;
    const RELEASED: u32 = 2;
    /// The startup hold, and the two gate blocks a manual threshold of 0 needs
    /// to open fully after it.
    const HOLD_FRAMES: usize = 20;
    const WARM_FRAMES: usize = HOLD_FRAMES + 2;

    fn frames(rate: i32) -> usize {
        (rate / 100) as usize
    }

    fn constant(rate: i32, value: f32) -> Vec<f32> {
        vec![value; frames(rate)]
    }

    fn run_input(h: &RootProcessorHarness, rate: i32, channels: usize, input: &[f32]) -> Vec<f32> {
        let frame = h.process(rate, channels, input);
        assert!(frame.copies_match);
        assert_eq!(frame.samples.len(), frames(rate));
        frame.samples
    }

    fn run(h: &RootProcessorHarness, rate: i32, channels: usize, value: f32) -> Vec<f32> {
        run_input(h, rate, channels, &constant(rate, value))
    }

    fn silent(samples: &[f32]) -> bool {
        samples.iter().all(|&v| v == 0.0)
    }

    fn started(rate: i32, channels: i32) -> cxx::UniquePtr<RootProcessorHarness> {
        let h = new_root_processor_harness();
        h.initialize(rate, channels);
        h
    }

    /// No voice gain and a manual threshold of 0: whatever is not silent opens
    /// the noise gate within two blocks and then passes unchanged.
    fn pass_through(rate: i32, channels: i32) -> cxx::UniquePtr<RootProcessorHarness> {
        let h = started(rate, channels);
        h.set_dsp(false, false, 0.0);
        h
    }

    /// pass_through, past the startup hold with the noise gate open.
    fn warmed(rate: i32, value: f32) -> cxx::UniquePtr<RootProcessorHarness> {
        let h = pass_through(rate, 1);
        for _ in 0..WARM_FRAMES {
            run(&h, rate, 1, value);
        }
        h
    }

    /// 300 Hz, continuous across frames from `*position` on.
    fn tone(rate: i32, amplitude: f32, position: &mut usize) -> Vec<f32> {
        let start = *position;
        *position += frames(rate);
        (0..frames(rate))
            .map(|i| {
                let t = (start + i) as f64 / rate as f64;
                amplitude * (2.0 * std::f64::consts::PI * 300.0 * t).sin() as f32
            })
            .collect()
    }

    #[test]
    fn root_capture_processor_passes_audio_through_after_the_startup_hold() {
        let h = pass_through(48000, 1);
        for _ in 0..HOLD_FRAMES {
            assert!(silent(&run(&h, 48000, 1, 1000.0)));
        }
        let opening = run(&h, 48000, 1, 1000.0);
        assert!(opening[0] > 0.0 && opening[0] < 1000.0, "{}", opening[0]);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
        let dsp = h.dsp(false);
        assert_eq!((dsp.startup_hold_frames, dsp.processed_frames), (20, 22));
        assert_eq!(
            (dsp.dsp_resets, dsp.dsp_flushes),
            (0, 1),
            "the first frame configures the chain"
        );
    }

    /// The gate's fade, 5 ms.
    fn fade(rate: i32) -> usize {
        (rate / 200) as usize
    }

    #[test]
    fn root_capture_processor_mute_zeroes_the_frame() {
        let h = pass_through(48000, 1);
        h.set_mic_muted(true);
        for _ in 0..WARM_FRAMES + 5 {
            assert!(silent(&run(&h, 48000, 1, 1000.0)), "a new stream starts shut");
        }
        h.set_mic_muted(false);
        run(&h, 48000, 1, 1000.0);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
    }

    #[test]
    fn root_capture_processor_gate_fades_over_5_ms_each_way() {
        for rate in [8000, 16000, 44100, 48000] {
            let h = warmed(rate, 1000.0);
            let f = fade(rate);
            run(&h, rate, 1, 1000.0);
            h.set_mic_muted(true);
            let closing = run(&h, rate, 1, 1000.0);
            assert!(closing.windows(2).all(|w| w[1] <= w[0]), "{rate}");
            assert!(closing[0] > 950.0 && closing[f - 2] > 0.0, "{rate}");
            assert!(closing[f..].iter().all(|&v| v == 0.0), "{rate}");
            assert!(run(&h, rate, 1, 1000.0).iter().all(|&v| v == 0.0), "{rate}");

            h.set_mic_muted(false);
            let opening = run(&h, rate, 1, 1000.0);
            assert!(opening.windows(2).all(|w| w[1] >= w[0]), "{rate}");
            assert!(opening[0] > 0.0 && opening[0] < 50.0, "{rate}: {}", opening[0]);
            assert!(opening[f..].iter().all(|&v| v == 1000.0), "{rate}");
        }
    }

    #[test]
    fn root_capture_processor_push_to_talk_gates_on_the_key() {
        let h = pass_through(48000, 1);
        h.set_push_to_talk(RELEASED);
        for _ in 0..WARM_FRAMES + 5 {
            assert!(silent(&run(&h, 48000, 1, 1000.0)));
        }
        h.set_push_to_talk(HELD);
        let pressed = run(&h, 48000, 1, 1000.0);
        assert!(pressed[0] < 10.0);
        assert!(pressed[fade(48000)..].iter().all(|&v| v == 1000.0));
        h.set_push_to_talk(VOICE_ACTIVITY);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
        h.set_push_to_talk(HELD);
        h.set_mic_muted(true);
        run(&h, 48000, 1, 1000.0);
        assert!(run(&h, 48000, 1, 1000.0).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn root_capture_processor_unmute_with_the_key_up_never_opens() {
        let h = pass_through(48000, 1);
        h.set_mic_muted(true);
        for _ in 0..WARM_FRAMES {
            run(&h, 48000, 1, 1000.0);
        }
        h.set_gate(0, RELEASED as i32);
        assert!(run(&h, 48000, 1, 1000.0).iter().all(|&v| v == 0.0));
        h.set_gate(-1, 9);
        assert!(run(&h, 48000, 1, 1000.0).iter().all(|&v| v == 0.0), "unknown values close");
        h.set_gate(-1, VOICE_ACTIVITY as i32);
        run(&h, 48000, 1, 1000.0);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
    }

    #[test]
    fn root_capture_processor_ramps_the_input_volume_over_20_ms() {
        let h = warmed(48000, 1.0);
        h.set_input_volume(0.0);
        let first = run(&h, 48000, 1, 1.0);
        let second = run(&h, 48000, 1, 1.0);
        let ramp: Vec<f32> = first.iter().chain(second.iter()).copied().collect();
        assert!(ramp.windows(2).all(|w| w[1] <= w[0]));
        assert!((first[479] - 0.5).abs() < 1e-3);
        assert_eq!(second[479], 0.0);
        assert!(run(&h, 48000, 1, 1.0).iter().all(|&v| v == 0.0));

        h.set_input_volume(2.0);
        run(&h, 48000, 1, 1.0);
        run(&h, 48000, 1, 1.0);
        assert_eq!(run(&h, 48000, 1, 1.0), vec![2.0; 480]);
    }

    #[test]
    fn root_capture_processor_ramp_is_20_ms_at_any_rate() {
        for rate in [8000, 16000, 32000, 44100] {
            let h = warmed(rate, 1.0);
            h.set_input_volume(0.0);
            let n = frames(rate);
            let first = run(&h, rate, 1, 1.0);
            assert!((first[n - 1] - 0.5).abs() < 1e-2, "{rate}: {}", first[n - 1]);
            assert_eq!(run(&h, rate, 1, 1.0)[n - 1], 0.0, "{rate}");
        }
    }

    #[test]
    fn root_capture_processor_clamps_and_sanitises_the_input_volume() {
        let h = warmed(16000, 1.0);
        h.set_input_volume(f32::NAN);
        run(&h, 16000, 1, 1.0);
        run(&h, 16000, 1, 1.0);
        assert!(run(&h, 16000, 1, 1.0).iter().all(|&v| v == 0.0));
        h.set_input_volume(9.0);
        run(&h, 16000, 1, 1.0);
        run(&h, 16000, 1, 1.0);
        assert_eq!(run(&h, 16000, 1, 1.0), vec![2.0; 160]);
    }

    #[test]
    fn root_capture_processor_handles_odd_rates_and_stereo_buffers() {
        for rate in [8000, 16000, 32000, 44100, 48000] {
            for channels in [1, 2] {
                let h = pass_through(rate, 1);
                let input: Vec<f32> = (0..frames(rate)).map(|i| i as f32).collect();
                let mut last = Vec::new();
                for _ in 0..WARM_FRAMES {
                    last = run_input(&h, rate, channels, &input);
                }
                assert_eq!(last, input, "{rate}/{channels}");
                let stats = h.stats();
                assert_eq!(stats.process_rate, rate as u32);
                assert_eq!(stats.process_channels, channels as u32);
                assert_eq!(stats.stage_samples, (WARM_FRAMES * frames(rate)) as u64);
                let dsp = h.dsp(false);
                assert_eq!((dsp.unsupported_frames, dsp.non_finite_samples), (0, 0));
            }
        }
    }

    #[test]
    fn root_capture_processor_mutes_every_channel() {
        let h = pass_through(48000, 1);
        h.set_mic_muted(true);
        for _ in 0..WARM_FRAMES + 2 {
            let frame = h.process(48000, 2, &constant(48000, 500.0));
            assert!(frame.copies_match);
            assert!(silent(&frame.samples));
        }
    }

    #[test]
    fn root_capture_processor_counts_frames_and_levels() {
        let h = warmed(48000, 100.0);
        let before = h.dsp(false);
        run(&h, 48000, 1, 100.0);
        h.set_mic_muted(true);
        run(&h, 48000, 1, 100.0);
        run(&h, 48000, 1, 100.0);
        let stats = h.stats();
        let dsp = h.dsp(false);
        assert_eq!(stats.processed_frames, WARM_FRAMES as u64 + 3);
        assert_eq!(stats.stage_samples, stats.processed_frames * 480);
        assert_eq!(stats.apm_out_sumsq, stats.stage_samples as f64 * 100.0 * 100.0);
        assert_eq!(
            (dsp.processed_frames, dsp.apm_out_sumsq),
            (stats.processed_frames, stats.apm_out_sumsq)
        );
        let level = 480.0 * 100.0 * 100.0;
        let sent = dsp.sent_sumsq - before.sent_sumsq;
        let faded = sent - level;
        assert!(faded > 0.0 && faded < level / 4.0, "{faded}");
        let close = |a: f64, b: f64| (a - b).abs() <= 1e-9 * b;
        assert!(close(dsp.denoised_sumsq - before.denoised_sumsq, 3.0 * level), "no denoiser");
        assert!(close(dsp.gate_in_sumsq - before.gate_in_sumsq, 3.0 * level), "no voice gain");
        assert_eq!(dsp.transmit_frames - before.transmit_frames, 2);
        assert_eq!(
            dsp.gate_open_frames - before.gate_open_frames,
            3,
            "the gate keeps running while muted"
        );
        assert!(dsp.gate_speaking && !dsp.speaking && !dsp.published_speaking);
    }

    #[test]
    fn root_capture_processor_replaces_non_finite_input() {
        let h = warmed(48000, 1000.0);
        let mut input = constant(48000, 1000.0);
        input[3] = f32::NAN;
        input[100] = f32::INFINITY;
        input[479] = f32::NEG_INFINITY;
        let out = run_input(&h, 48000, 1, &input);
        assert_eq!((out[3], out[100], out[479]), (0.0, 0.0, 0.0));
        assert!(out.iter().enumerate().all(|(i, &v)| [3, 100, 479].contains(&i) || v == 1000.0));
        assert_eq!(h.dsp(false).non_finite_samples, 3);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
    }

    #[test]
    fn root_capture_processor_meters_unsupported_shapes_and_only_gates_them() {
        let h = pass_through(96000, 1);
        let input: Vec<f32> = (0..960).map(|i| (i % 7) as f32 * 100.0).collect();
        for _ in 0..3 {
            assert_eq!(run_input(&h, 96000, 1, &input), input, "no startup hold, no gate");
        }
        let dsp = h.dsp(true);
        let sumsq: f64 = input.iter().map(|&v| v as f64 * v as f64).sum();
        assert_eq!((dsp.unsupported_frames, dsp.processed_frames, dsp.stage_samples), (3, 3, 2880));
        assert_eq!(dsp.apm_out_sumsq, 3.0 * sumsq);
        assert_eq!(dsp.sent_sumsq, 3.0 * sumsq);
        assert_eq!((dsp.denoised_sumsq, dsp.gate_in_sumsq), (3.0 * sumsq, 3.0 * sumsq));
        assert_eq!((dsp.startup_hold_frames, dsp.gate_open_frames, dsp.transmit_frames), (0, 0, 3));
        assert!(dsp.gate_in_block_peak_db.is_nan(), "no gate block ran");
        h.set_mic_muted(true);
        assert!(silent(&run_input(&h, 96000, 1, &input)));
        assert_eq!(h.dsp(false).transmit_frames, 3);
        h.set_mic_muted(false);
        assert_eq!(run_input(&h, 96000, 1, &input), input);
    }

    #[test]
    fn root_capture_processor_reports_speaking_and_releases_it_after_the_gate() {
        let h = started(48000, 1);
        let mut position = 0;
        let mut first_speaking = None;
        for i in 0..60 {
            run_input(&h, 48000, 1, &tone(48000, 3000.0, &mut position));
            if first_speaking.is_none() && h.dsp(false).published_speaking {
                first_speaking = Some(i);
            }
        }
        let first = first_speaking.expect("speech opens the gate");
        assert!((HOLD_FRAMES..HOLD_FRAMES + 3).contains(&first), "{first}");
        let dsp = h.dsp(false);
        assert!(dsp.speaking && dsp.gate_speaking);
        assert_eq!((dsp.published_seq, dsp.speaking_seq), (1, 1));

        let mut silent_frames = 0;
        while h.dsp(false).published_speaking {
            run(&h, 48000, 1, 0.0);
            silent_frames += 1;
            assert!(silent_frames < 100);
        }
        // 400 ms release, then the 300 ms hold.
        assert!((68..=72).contains(&silent_frames), "{silent_frames}");
        assert_eq!(h.dsp(false).published_seq, 2);
    }

    #[test]
    fn root_capture_processor_reports_no_speaking_while_muted() {
        let h = started(48000, 1);
        h.set_mic_muted(true);
        let mut position = 0;
        for _ in 0..60 {
            let out = run_input(&h, 48000, 1, &tone(48000, 3000.0, &mut position));
            assert!(silent(&out));
        }
        let dsp = h.dsp(false);
        assert!(dsp.gate_speaking, "the gate keeps listening while muted");
        assert!(!dsp.speaking && !dsp.published_speaking);
        assert_eq!(dsp.published_seq, 0);
        assert_eq!(dsp.transmit_frames, 0);

        h.set_mic_muted(false);
        let out = run_input(&h, 48000, 1, &tone(48000, 3000.0, &mut position));
        assert!(!silent(&out));
        let dsp = h.dsp(false);
        assert!(dsp.published_speaking, "unmuting while talking speaks at once");
        assert_eq!(dsp.published_seq, 1);
        h.set_mic_muted(true);
        run_input(&h, 48000, 1, &tone(48000, 3000.0, &mut position));
        assert!(!h.dsp(false).published_speaking);
        assert_eq!(h.dsp(false).published_seq, 2);
    }

    #[test]
    fn root_capture_processor_push_to_talk_press_is_heard_within_a_frame() {
        let h = started(48000, 1);
        h.set_dsp(false, false, 0.01);
        h.set_push_to_talk(RELEASED);
        for _ in 0..WARM_FRAMES + 5 {
            assert!(silent(&run(&h, 48000, 1, 1000.0)));
        }
        assert!(!h.dsp(false).published_speaking);
        h.set_push_to_talk(HELD);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let pressed = run(&h, 48000, 1, 1000.0);
        let f = fade(48000);
        assert!(pressed[0] > 0.0 && pressed[0] < 10.0, "{}", pressed[0]);
        assert!(pressed[f..].iter().all(|&v| v == 1000.0));
        let dsp = h.dsp(false);
        assert!(dsp.published_speaking);
        assert!(dsp.gate_open_delay_us >= 2000, "{}", dsp.gate_open_delay_us);

        h.set_push_to_talk(RELEASED);
        let released = run(&h, 48000, 1, 1000.0);
        assert!(released[0] > 990.0, "the key-up fades instead of clicking: {}", released[0]);
        assert!(released.windows(2).all(|w| w[1] <= w[0]));
        assert!(silent(&released[f..]));
        assert!(!h.dsp(false).published_speaking, "key up stops speaking at once");
        assert!(silent(&run(&h, 48000, 1, 1000.0)));
    }

    #[test]
    fn root_capture_processor_push_to_talk_in_auto_sensitivity_opens_on_the_fifth_block() {
        let h = started(48000, 1);
        h.set_dsp(false, true, 0.025);
        h.set_push_to_talk(RELEASED);
        for _ in 0..WARM_FRAMES + 5 {
            run(&h, 48000, 1, 1000.0);
        }
        h.set_push_to_talk(HELD);
        assert!(silent(&run(&h, 48000, 1, 1000.0)), "four blocks of 2.7 ms are a click");
        let second = run(&h, 48000, 1, 1000.0);
        assert!((second[0] - 1000.0 * 2.0 / 3.0).abs() < 1.0, "{}", second[0]);
        assert!(second[128..].iter().all(|&v| v == 1000.0));
        assert!(h.dsp(false).published_speaking);
    }

    #[test]
    fn root_capture_processor_gain_reset_restarts_the_voice_gain() {
        let h = started(48000, 1);
        h.gain_reset(8.0);
        run(&h, 48000, 1, 0.0);
        let dsp = h.dsp(false);
        assert_eq!(dsp.voice_gain_db, 8.0);
        assert!(!dsp.voice_gain_settled);

        h.gain_reset(30.0);
        run(&h, 48000, 1, 0.0);
        assert_eq!(h.dsp(false).voice_gain_db, 24.0, "clamped to the voice gain's maximum");
        h.gain_reset(f32::NAN);
        run(&h, 48000, 1, 0.0);
        assert_eq!(h.dsp(false).voice_gain_db, 24.0, "a non-finite reset is ignored");

        h.set_dsp(false, true, 0.025);
        run(&h, 48000, 1, 0.0);
        assert_eq!(h.dsp(false).voice_gain_db, 24.0, "off keeps the last value");
        h.gain_reset(3.0);
        h.set_dsp(true, true, 0.025);
        run(&h, 48000, 1, 0.0);
        assert_eq!(h.dsp(false).voice_gain_db, 3.0, "back on, from the newest reset");
    }

    #[test]
    fn root_capture_processor_restarts_the_startup_hold_on_a_new_stream_and_a_reset() {
        let h = warmed(48000, 1000.0);
        let before = h.dsp(false);
        h.bump_stream_epoch();
        for _ in 0..HOLD_FRAMES {
            assert!(silent(&run(&h, 48000, 1, 1000.0)));
        }
        run(&h, 48000, 1, 1000.0);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
        let dsp = h.dsp(false);
        assert_eq!(dsp.dsp_flushes - before.dsp_flushes, 1);
        assert_eq!(dsp.startup_hold_frames - before.startup_hold_frames, HOLD_FRAMES as u64);
        assert_eq!(dsp.dsp_resets, 0);

        h.reset_controls();
        h.set_dsp(false, false, 0.0);
        for _ in 0..HOLD_FRAMES {
            assert!(silent(&run(&h, 48000, 1, 1000.0)));
        }
        let dsp = h.dsp(false);
        assert_eq!(dsp.dsp_resets, 1);
        assert_eq!(dsp.dsp_flushes - before.dsp_flushes, 2, "a reset flushes too");
    }

    #[test]
    fn root_capture_processor_runs_the_chain_at_16_and_8_khz() {
        for rate in [16000, 8000] {
            let h = started(rate, 1);
            let mut position = 0;
            let mut out = Vec::new();
            for _ in 0..40 {
                out = run_input(&h, rate, 1, &tone(rate, 3000.0, &mut position));
            }
            assert!(out.iter().any(|&v| v.abs() > 1000.0), "{rate}");
            let dsp = h.dsp(true);
            assert!(dsp.published_speaking, "{rate}");
            assert_eq!(dsp.stage_samples, 40 * frames(rate) as u64, "{rate}");
            let peak_db = dsp.gate_in_block_peak_db;
            assert!(peak_db > -30.0 && peak_db < -12.0, "{rate}: {peak_db}");
            assert!(dsp.envelope_max > 0.9, "{rate}");
            assert!(dsp.threshold > 0.0 && dsp.threshold < 0.1, "{rate}: {}", dsp.threshold);
            assert!(h.dsp(false).gate_in_block_peak_db.is_nan(), "taken");
        }
    }

    #[test]
    fn root_capture_processor_bumps_the_format_sequence_only_on_a_change() {
        let h = started(48000, 1);
        assert_eq!(h.stats().format_seq, 0);
        run(&h, 48000, 1, 1.0);
        assert_eq!(h.stats().format_seq, 1);
        h.initialize(48000, 1);
        run(&h, 48000, 1, 1.0);
        assert_eq!(h.stats().format_seq, 1);
        run(&h, 48000, 2, 1.0);
        assert_eq!(h.stats().format_seq, 2);
        h.initialize(16000, 1);
        run(&h, 16000, 2, 1.0);
        let stats = h.stats();
        assert_eq!((stats.format_seq, stats.process_rate, stats.process_channels), (3, 16000, 2));
    }

    #[test]
    fn root_capture_processor_takes_the_rate_from_the_buffer_when_initialize_disagrees() {
        let h = started(48000, 1);
        run(&h, 16000, 1, 1.0);
        assert_eq!(h.stats().process_rate, 16000);
    }

    #[test]
    fn root_capture_processor_runs_the_denoiser_between_the_chain_stages() {
        let h = pass_through(48000, 1);
        h.set_denoiser_wanted(true);
        h.set_denoise_power(0.4);
        assert!(h.install_fake_denoiser(48000, 0.5, 0, 0, 0, 0));
        let mut out = Vec::new();
        for _ in 0..WARM_FRAMES {
            out = run(&h, 48000, 1, 1000.0);
        }
        assert_eq!(out, vec![500.0; 480]);
        let fake = h.fake_denoiser();
        assert_eq!(fake.process_calls, WARM_FRAMES as u64, "the denoiser runs under the hold too");
        assert_eq!((fake.flush_calls, fake.power_sets, fake.last_power), (0, 1, 0.4));
        assert_eq!(fake.bypass, 0);
        let dsp = h.dsp(false);
        assert_eq!(dsp.ns_gap_frames, 0);
        assert!(dsp.aligner_primed);
        let denoised = WARM_FRAMES as f64 * 480.0 * 500.0 * 500.0;
        assert!((dsp.denoised_sumsq - denoised).abs() <= 1e-9 * denoised, "{}", dsp.denoised_sumsq);

        h.set_denoise_power(0.8);
        h.bump_stream_epoch();
        assert!(silent(&run(&h, 48000, 1, 1000.0)), "a new stream holds again");
        let fake = h.fake_denoiser();
        assert_eq!((fake.flush_calls, fake.power_sets, fake.last_power), (1, 2, 0.8));
    }

    #[test]
    fn root_capture_processor_bypasses_a_stalled_denoiser_and_counts_the_gap() {
        let h = pass_through(48000, 1);
        h.set_denoiser_wanted(true);
        assert!(h.install_fake_denoiser(48000, 0.5, 0, 0, 1, 1000));
        for _ in 0..100 {
            assert!(silent(&run(&h, 48000, 1, 1000.0)), "an empty denoiser sends silence");
        }
        let fake = h.fake_denoiser();
        assert_eq!((fake.process_calls, fake.bypass), (100, 2), "stalled after 100 empty outputs");
        assert_eq!(h.dsp(false).ns_gap_frames, 0);

        run(&h, 48000, 1, 1000.0);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480], "bypassed: dry");
        assert_eq!(h.fake_denoiser().process_calls, 100);
        assert_eq!(h.dsp(false).ns_gap_frames, 2);
        h.set_webrtc_ns_applied(true);
        run(&h, 48000, 1, 1000.0);
        assert_eq!(h.dsp(false).ns_gap_frames, 2, "WebRTC NS covers it");
        h.set_webrtc_ns_applied(false);
        h.set_denoiser_wanted(false);
        run(&h, 48000, 1, 1000.0);
        assert_eq!(h.dsp(false).ns_gap_frames, 2, "not wanted, no gap");
    }

    #[test]
    fn root_capture_processor_denoises_at_16_khz_and_bypasses_a_pipeline_at_another_rate() {
        let h = pass_through(16000, 1);
        h.set_denoiser_wanted(true);
        assert!(h.install_fake_denoiser(16000, 0.5, 0, 0, 0, 0));
        let mut out = Vec::new();
        for _ in 0..WARM_FRAMES {
            out = run(&h, 16000, 1, 1000.0);
        }
        assert_eq!(out, vec![500.0; 160]);

        assert!(h.install_fake_denoiser(48000, 0.5, 0, 0, 0, 0));
        assert_eq!(run(&h, 16000, 1, 1000.0), vec![1000.0; 160], "a 48 kHz pipeline is bypassed");
        let fake = h.fake_denoiser();
        assert_eq!((fake.process_calls, fake.bypass), (0, 1));
    }

    #[test]
    fn root_capture_processor_passes_a_missed_frame_dry_and_counts_the_gap() {
        let h = pass_through(48000, 1);
        h.set_denoiser_wanted(true);
        assert!(h.install_fake_denoiser(48000, 0.5, 0, 0, 0, 0));
        for _ in 0..WARM_FRAMES {
            run(&h, 48000, 1, 1000.0);
        }
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![500.0; 480]);
        let before = h.dsp(false);
        assert!(h.hold_denoiser_slot(true));
        let missed = run(&h, 48000, 1, 1000.0);
        assert!(h.hold_denoiser_slot(false));
        assert_eq!(missed, vec![1000.0; 480], "dry, neither held nor silent");
        let fake = h.fake_denoiser();
        assert_eq!((fake.misses, fake.process_calls), (1, WARM_FRAMES as u64 + 1));
        let dsp = h.dsp(false);
        assert_eq!(dsp.ns_gap_frames - before.ns_gap_frames, 1, "nothing denoised it");
        assert_eq!(dsp.startup_hold_frames, before.startup_hold_frames);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![500.0; 480], "the aligner carries on");
    }

    #[test]
    fn root_capture_processor_restarts_on_a_pipeline_replaced_mid_stream() {
        let h = pass_through(48000, 1);
        h.set_denoiser_wanted(true);
        assert!(h.install_fake_denoiser(48000, 0.5, 240, 0, 0, 0));
        for _ in 0..WARM_FRAMES + 3 {
            run(&h, 48000, 1, 1000.0);
        }
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![500.0; 480]);
        let before = h.dsp(false);
        assert!(h.install_fake_denoiser(48000, 0.25, 0, 0, 0, 0));
        let replaced = run(&h, 48000, 1, 1000.0);
        assert!(replaced.iter().all(|&v| v == 250.0 || v == 0.0), "nothing of the old one");
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![250.0; 480]);
        let fake = h.fake_denoiser();
        assert_eq!((fake.process_calls, fake.flush_calls, fake.power_sets), (2, 0, 1));
        let dsp = h.dsp(false);
        assert_eq!(dsp.dsp_flushes, before.dsp_flushes, "a replacement is no stream flush");
        assert_eq!(dsp.ns_gap_frames, before.ns_gap_frames);
    }

    #[test]
    fn root_capture_processor_a_rate_change_bypasses_the_pipeline_until_the_rate_is_back() {
        let h = pass_through(48000, 1);
        h.set_denoiser_wanted(true);
        assert!(h.install_fake_denoiser(48000, 0.5, 0, 0, 0, 0));
        for _ in 0..WARM_FRAMES {
            run(&h, 48000, 1, 1000.0);
        }
        let before = h.dsp(false);

        h.initialize(16000, 1);
        let mut first_audible = None;
        for i in 0..110 {
            if !silent(&run(&h, 16000, 1, 1000.0)) && first_audible.is_none() {
                first_audible = Some(i);
            }
        }
        let first = first_audible.expect("dry audio after the hold");
        assert!((HOLD_FRAMES..=101).contains(&first), "{first}");
        let fake = h.fake_denoiser();
        assert_eq!((fake.bypass, fake.process_calls), (1, WARM_FRAMES as u64));
        let dsp = h.dsp(false);
        assert_eq!(dsp.dsp_flushes - before.dsp_flushes, 1, "the rate change");
        assert!(dsp.ns_gap_frames - before.ns_gap_frames >= 9, "dry after the hold: a gap");
        assert_eq!(run(&h, 16000, 1, 1000.0), vec![1000.0; 160]);

        h.initialize(48000, 1);
        for _ in 0..HOLD_FRAMES {
            assert!(silent(&run(&h, 48000, 1, 1000.0)));
        }
        let fake = h.fake_denoiser();
        assert_eq!(fake.bypass, 0, "the rate is back");
        assert_eq!(fake.flush_calls, 1, "it starts over");
        run(&h, 48000, 1, 1000.0);
        run(&h, 48000, 1, 1000.0);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![500.0; 480]);
    }

    #[test]
    fn root_capture_processor_runs_the_denoiser_only_while_wanted() {
        let h = pass_through(48000, 1);
        assert!(h.install_fake_denoiser(48000, 0.5, 0, 0, 0, 0));
        let mut out = Vec::new();
        for _ in 0..WARM_FRAMES {
            out = run(&h, 48000, 1, 1000.0);
        }
        assert_eq!(h.fake_denoiser().process_calls, 0, "not wanted: never fed");
        assert_eq!(out, vec![1000.0; 480], "dry, and no 100-frame wait for it");

        h.set_denoiser_wanted(true);
        run(&h, 48000, 1, 1000.0);
        assert_eq!(h.fake_denoiser().process_calls, 1);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![500.0; 480]);
        h.set_denoiser_wanted(false);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
        assert_eq!(h.fake_denoiser().process_calls, 2);
        h.set_denoiser_wanted(true);
        run(&h, 48000, 1, 1000.0);
        let fake = h.fake_denoiser();
        assert_eq!((fake.process_calls, fake.flush_calls), (3, 1), "it missed frames: flushed");
    }

    #[test]
    fn root_capture_processor_without_a_slot_never_waits_for_a_denoiser() {
        let h = new_root_processor_harness_without_slot();
        h.initialize(48000, 1);
        h.set_dsp(false, false, 0.0);
        h.set_denoiser_wanted(true);
        let mut out = Vec::new();
        for _ in 0..WARM_FRAMES {
            out = run(&h, 48000, 1, 1000.0);
        }
        assert_eq!(out, vec![1000.0; 480]);
        let dsp = h.dsp(false);
        assert_eq!((dsp.startup_hold_frames, dsp.ns_gap_frames), (HOLD_FRAMES as u64, 0));
    }

    #[test]
    fn root_capture_processor_a_reset_clears_a_stale_speaking_flag() {
        let h = started(48000, 1);
        h.set_published_speaking(true);
        let seq = h.dsp(false).published_seq;
        h.reset_controls();
        let dsp = h.dsp(false);
        assert!(!dsp.published_speaking && !dsp.speaking, "cleared without a frame");
        assert_eq!(dsp.published_seq, seq + 1);
        h.reset_controls();
        assert_eq!(h.dsp(false).published_seq, seq + 1, "already clear: no transition");
    }

    #[test]
    fn root_capture_processor_speaking_goes_stale_when_frames_stop() {
        let h = started(48000, 1);
        h.set_published_speaking(true);
        assert!(h.audibly_speaking());
        h.clear_stale_speaking();
        assert!(h.dsp(false).published_speaking, "a frame just now");
        std::thread::sleep(std::time::Duration::from_millis(320));
        assert!(!h.audibly_speaking(), "no frame for 300 ms: nobody is heard");
        assert!(h.dsp(false).published_speaking, "published until the control side clears it");
        let seq = h.dsp(false).published_seq;
        h.clear_stale_speaking();
        let dsp = h.dsp(false);
        assert!(!dsp.published_speaking);
        assert_eq!(dsp.published_seq, seq + 1);
    }

    #[test]
    fn root_capture_processor_counts_frames_the_ns_fallback_waits_for_the_apm() {
        let h = pass_through(48000, 1);
        for _ in 0..3 {
            run(&h, 48000, 1, 1000.0);
        }
        assert_eq!(h.dsp(false).ns_fallback_pending_frames, 0);
        h.set_ns_fallback_decided(true);
        h.set_mic_muted(true);
        run(&h, 48000, 1, 1000.0);
        run(&h, 48000, 1, 1000.0);
        assert_eq!(
            h.dsp(false).ns_fallback_pending_frames,
            2,
            "decided, not applied, muted or not"
        );
        h.set_webrtc_ns_applied(true);
        run(&h, 48000, 1, 1000.0);
        assert_eq!(h.dsp(false).ns_fallback_pending_frames, 2, "applied");
        h.set_ns_fallback_decided(false);
        h.set_webrtc_ns_applied(false);
        run(&h, 48000, 1, 1000.0);
        assert_eq!(h.dsp(false).ns_fallback_pending_frames, 2, "not decided");
    }

    #[test]
    fn root_capture_processor_publishes_against_the_stored_flag() {
        let h = pass_through(48000, 1);
        h.set_published_speaking(true);
        let seq = h.dsp(false).published_seq;
        run(&h, 48000, 1, 0.0);
        let dsp = h.dsp(false);
        assert!(!dsp.published_speaking, "silence clears whatever another writer left");
        assert_eq!(dsp.published_seq, seq + 1);
        run(&h, 48000, 1, 0.0);
        assert_eq!(h.dsp(false).published_seq, seq + 1);
    }

    #[test]
    fn root_capture_processor_reset_forgets_a_push_to_talk_press_no_frame_saw() {
        let h = started(48000, 1);
        h.set_push_to_talk(RELEASED);
        h.set_push_to_talk(HELD);
        assert_ne!(h.gate_open_stamp_us(), 0);
        h.reset_controls();
        assert_eq!(h.gate_open_stamp_us(), 0);
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.set_dsp(false, false, 0.0);
        for _ in 0..WARM_FRAMES {
            run(&h, 48000, 1, 1000.0);
        }
        assert_eq!(h.dsp(false).gate_open_delay_us, 0, "no delay from the last session");
    }

    #[test]
    fn root_capture_processor_is_not_audibly_speaking_inside_the_startup_hold() {
        let h = started(48000, 1);
        h.set_push_to_talk(HELD);
        let mut position = 0;
        for _ in 0..5 {
            run_input(&h, 48000, 1, &tone(48000, 3000.0, &mut position));
        }
        assert_eq!(h.dsp(false).startup_hold_frames, 5);
        assert!(!h.audibly_speaking(), "nothing is heard inside the hold");
        for _ in 0..30 {
            run_input(&h, 48000, 1, &tone(48000, 3000.0, &mut position));
        }
        assert!(h.dsp(false).published_speaking && h.audibly_speaking());
    }

    #[test]
    fn root_capture_processor_reset_opens_the_gate() {
        let h = started(48000, 1);
        h.set_mic_muted(true);
        h.set_push_to_talk(RELEASED);
        h.set_input_volume(0.5);
        h.reset_controls();
        h.set_dsp(false, false, 0.0);
        for _ in 0..WARM_FRAMES {
            run(&h, 48000, 1, 1.0);
        }
        assert_eq!(run(&h, 48000, 1, 1.0), vec![1.0; 480]);
    }

    fn threshold_db(h: &RootProcessorHarness) -> f64 {
        20.0 * f64::from(h.dsp(false).threshold).log10()
    }

    /// The decorator's flag, set by hand here: a digitally silent frame teaches auto sensitivity
    /// nothing, and the processor takes the flag, so the next frame without one is a level again.
    #[test]
    fn root_capture_processor_learns_nothing_from_a_frame_flagged_silent_and_takes_the_flag() {
        let h = started(48000, 1);
        h.set_dsp(false, true, 0.025);
        let room = 32768.0 * 10f32.powf(-60.0 / 20.0);
        for _ in 0..300 {
            run(&h, 48000, 1, room);
        }
        let learned = h.dsp(false).threshold;
        assert!((threshold_db(&h) + 52.0).abs() < 0.5, "{}", threshold_db(&h));
        for _ in 0..300 {
            h.set_capture_silent(true);
            run(&h, 48000, 1, 0.0);
        }
        assert_eq!(h.dsp(false).threshold, learned);
        run(&h, 48000, 1, 0.0);
        assert!(h.dsp(false).threshold < learned, "an unflagged frame of zeros is a level");
    }

    #[test]
    fn root_capture_processor_applies_the_auto_sensitivity_minimum_it_is_given() {
        let h = started(48000, 1);
        h.set_dsp(false, true, 0.025);
        for (setting, expected) in
            [(f32::NAN, -54.0), (-40.0, -40.0), (-100.0, -80.0), (-20.0, -30.0), (-54.0, -54.0)]
        {
            h.set_auto_sensitivity_minimum_db(setting);
            for _ in 0..300 {
                run(&h, 48000, 1, 0.0);
            }
            assert!((threshold_db(&h) - expected).abs() < 1e-3, "{setting}: {}", threshold_db(&h));
        }
        h.set_auto_sensitivity_minimum_db(-40.0);
        h.reset_controls();
        assert!(h.dsp(false).auto_sensitivity_minimum_db.is_nan(), "a reset forgets it");
        h.set_dsp(false, true, 0.025);
        for _ in 0..300 {
            run(&h, 48000, 1, 0.0);
        }
        assert!((threshold_db(&h) + 54.0).abs() < 1e-3, "{}", threshold_db(&h));
    }

    fn tuning(max_gain_db: f32) -> RootVoiceGainTuning {
        RootVoiceGainTuning {
            max_gain_db,
            target_db: f32::NAN,
            rise_db_per_second: f32::NAN,
            fall_db_per_second: f32::NAN,
            level_time_constant_ms: f32::NAN,
            min_speech_probability: f32::NAN,
        }
    }

    /// The stored tunables reach the chain on the next frame, before that frame's gain reset (so
    /// a reset is clamped to the maximum it arrives with), and a reset of the controls forgets them.
    #[test]
    fn root_capture_processor_applies_the_voice_gain_tuning_it_is_given() {
        let h = started(48000, 1);
        h.set_dsp(true, true, 0.025);
        for (max, reset, expected) in
            [(f32::NAN, 30.0f32, 24.0f32), (6.0, 17.0, 6.0), (40.0, 17.0, 17.0), (-3.0, 17.0, 0.0)]
        {
            h.set_voice_gain_tuning(tuning(max));
            h.gain_reset(reset);
            run(&h, 48000, 1, 0.0);
            assert_eq!(h.dsp(false).voice_gain_db, expected, "max {max}");
        }
        let stored = h.dsp(false).voice_gain;
        assert_eq!(stored.max_gain_db, -3.0, "stored as set, clamped by the chain");
        assert!(stored.target_db.is_nan());
        h.reset_controls();
        assert!(h.dsp(false).voice_gain.max_gain_db.is_nan(), "a reset forgets them");
        h.set_dsp(true, true, 0.025);
        h.gain_reset(30.0);
        run(&h, 48000, 1, 0.0);
        assert_eq!(h.dsp(false).voice_gain_db, 24.0);

        let applied = root_voice_gain_tuning(RootVoiceGainTuning {
            max_gain_db: 40.0,
            target_db: f32::NEG_INFINITY,
            rise_db_per_second: 0.1,
            fall_db_per_second: 25.0,
            level_time_constant_ms: 6000.0,
            min_speech_probability: 0.75,
        });
        assert_eq!(
            applied,
            RootVoiceGainTuning {
                max_gain_db: 24.0,
                target_db: -25.0,
                rise_db_per_second: 0.5,
                fall_db_per_second: 20.0,
                level_time_constant_ms: 6000.0,
                min_speech_probability: 0.75,
            }
        );
    }
}

/// Real speech for AGC2 (its speech detector wants a voice, not a tone).
#[cfg(test)]
pub(crate) mod test_speech {
    /// examples/play_from_disk/change-sophie.wav: about 5.9 s of a voice,
    /// 48 kHz mono int16, as FloatS16.
    pub fn sophie() -> Vec<f32> {
        let bytes: &[u8] = include_bytes!("../../examples/play_from_disk/change-sophie.wav");
        let mut at = 12;
        while at + 8 <= bytes.len() {
            let len =
                u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
                    as usize;
            if &bytes[at..at + 4] == b"data" {
                let end = (at + 8 + len).min(bytes.len());
                return bytes[at + 8..end]
                    .chunks_exact(2)
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])))
                    .collect();
            }
            at += 8 + len + (len & 1);
        }
        panic!("change-sophie.wav has no data chunk");
    }

    /// 48 kHz samples at `rate` (linear interpolation).
    pub fn resample(samples: &[f32], rate: i32) -> Vec<f32> {
        if rate == 48_000 {
            return samples.to_vec();
        }
        let step = 48_000.0 / f64::from(rate);
        let count = ((samples.len() - 1) as f64 / step) as usize;
        (0..count)
            .map(|i| {
                let x = i as f64 * step;
                let k = x as usize;
                let t = (x - k as f64) as f32;
                samples[k] * (1.0 - t) + samples[(k + 1).min(samples.len() - 1)] * t
            })
            .collect()
    }

    pub fn power(frame: &[f32]) -> f64 {
        frame.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / frame.len().max(1) as f64
    }

    pub fn dbfs(power: f64) -> f64 {
        10.0 * (power / (32768.0 * 32768.0)).max(1e-30).log10()
    }

    /// The voice's level: the mean power of its frames within 20 dB of the
    /// loudest, in dBFS.
    pub fn speech_db(samples: &[f32], n: usize) -> f64 {
        let levels: Vec<f64> = samples.chunks_exact(n).map(power).collect();
        let loudest = levels.iter().cloned().fold(0.0, f64::max);
        let active: Vec<f64> = levels.into_iter().filter(|&p| p > loudest * 0.01).collect();
        dbfs(active.iter().sum::<f64>() / active.len() as f64)
    }

    /// The clip at `rate` with its voice at `db` dBFS.
    pub fn at_level(rate: i32, db: f64) -> Vec<f32> {
        let clip = resample(&sophie(), rate);
        let n = (rate / 100) as usize;
        let scale = 10f64.powf((db - speech_db(&clip, n)) / 20.0) as f32;
        clip.into_iter().map(|s| s * scale).collect()
    }
}

/// libwebrtc's AGC2 as the voice gain (gain engine 1) in the capture
/// processor, with the control side's builds as the controller makes them.
#[cfg(test)]
mod agc2_tests {
    use super::ffi::{new_root_processor_harness, RootAgc2Tuning, RootProcessorHarness};
    use super::test_speech::{at_level, dbfs, power, speech_db};

    const AGC2: u32 = 1;
    const RATES: [i32; 8] = [8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000];

    fn frames(rate: i32) -> usize {
        (rate / 100) as usize
    }

    /// AGC2 from a gain reset of `reset_db`, built for `rate` as the poll
    /// does once the processor runs at it.
    fn agc2(
        rate: i32,
        reset_db: f32,
        auto_sensitivity: bool,
    ) -> cxx::UniquePtr<RootProcessorHarness> {
        agc2_with(rate, reset_db, auto_sensitivity, tuning(f32::NAN, f32::NAN))
    }

    fn tuning(max_gain_db: f32, max_output_noise_dbfs: f32) -> RootAgc2Tuning {
        RootAgc2Tuning {
            headroom_db: f32::NAN,
            max_gain_db,
            max_gain_change_db_per_second: f32::NAN,
            max_output_noise_dbfs,
        }
    }

    fn agc2_with(
        rate: i32,
        reset_db: f32,
        auto_sensitivity: bool,
        settings: RootAgc2Tuning,
    ) -> cxx::UniquePtr<RootProcessorHarness> {
        let h = new_root_processor_harness();
        h.initialize(rate, 1);
        h.set_dsp(true, auto_sensitivity, 0.0);
        h.set_agc2_tuning(settings);
        h.gain_reset(reset_db);
        h.set_gain_engine(AGC2);
        h.process(rate, 1, &vec![0.0; frames(rate)]);
        h.service_agc2();
        h
    }

    fn tone(rate: i32, db: f64, position: &mut usize) -> Vec<f32> {
        let a = (32768.0 * 10f64.powf(db / 20.0) * 2f64.sqrt()) as f32;
        let start = *position;
        *position += frames(rate);
        (0..frames(rate))
            .map(|i| {
                a * (2.0 * std::f32::consts::PI * 300.0 * (start + i) as f32 / rate as f32).sin()
            })
            .collect()
    }

    /// The output level of the voice's frames (within 20 dB of the loudest),
    /// their average level in dB as the chain's target counts it, once AGC2
    /// had `settle` passes of the clip, over `passes` more.
    fn talker_out_db(
        h: &RootProcessorHarness,
        rate: i32,
        clip: &[f32],
        settle: usize,
        passes: usize,
    ) -> f64 {
        let n = frames(rate);
        let loudest = clip.chunks_exact(n).map(power).fold(0.0, f64::max);
        let (mut sum, mut count) = (0.0, 0usize);
        for pass in 0..settle + passes {
            for frame in clip.chunks_exact(n) {
                let out = h.process(rate, 1, frame).samples;
                if pass >= settle && power(frame) > loudest * 0.01 {
                    sum += dbfs(power(&out));
                    count += 1;
                }
            }
            h.service_agc2();
        }
        sum / count as f64
    }

    /// A quiet talker (-45 dBFS) comes out at about -25 dBFS, from 0 dB and
    /// at 48, 44.1 and 16 kHz (the processor's rate for 44.1 kHz devices is
    /// 48 kHz, but AGC2 also runs on a 44.1 kHz frame).
    #[test]
    fn root_capture_processor_agc2_brings_a_quiet_talker_to_about_minus_25_dbfs() {
        for rate in [48000, 44100, 16000] {
            let clip = at_level(rate, -45.0);
            assert!((speech_db(&clip, frames(rate)) + 45.0).abs() < 0.01);
            let h = agc2(rate, 0.0, true);
            let out = talker_out_db(&h, rate, &clip, 4, 2);
            let dsp = h.dsp(false);
            assert!(
                (out + 25.0).abs() <= 3.0,
                "{rate}: {out:.1} dBFS at {:.1} dB",
                dsp.voice_gain_db
            );
            assert_eq!((dsp.gain_engine_applied, dsp.agc2_rate), (AGC2, rate as u32), "{rate}");
            assert!(dsp.voice_gain_settled, "{rate}");
        }
    }

    /// Room noise never opens the gate at 20-30 dB of AGC2 gain: auto
    /// sensitivity learns the floor before the gain and adds the gain AGC2
    /// applied, and its -54 dBFS minimum gives way only to the room's floor
    /// plus 8 dB. The room is a fan drifting ±3 dB over 2 s.
    #[test]
    fn root_capture_processor_agc2_room_noise_never_opens_the_gate_at_20_to_30_db() {
        for (reset, room_db) in [(20.0f32, -75.0f64), (25.0, -80.0), (30.0, -85.0), (30.0, -70.0)] {
            let h = agc2(48000, reset, true);
            let mut seed = 0x2545_F491_4F6C_DD1Du64;
            let mut open_after_learning = 0;
            let mut gains = Vec::new();
            for f in 0..3000 {
                let db = room_db + 3.0 * (2.0 * std::f64::consts::PI * f as f64 / 200.0).sin();
                let a = (32768.0 * 10f64.powf(db / 20.0) * 3f64.sqrt()) as f32;
                let frame: Vec<f32> = (0..480)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        a * ((seed >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0)
                    })
                    .collect();
                h.process(48000, 1, &frame);
                if f == 500 {
                    open_after_learning = h.dsp(false).gate_open_frames;
                }
                if f >= 500 {
                    gains.push(h.dsp(false).voice_gain_db);
                }
            }
            let dsp = h.dsp(false);
            let lowest = gains.iter().cloned().fold(f32::MAX, f32::min);
            assert!(lowest >= 19.5 || room_db > -75.0, "{reset} dB over {room_db}: {lowest}");
            assert_eq!(
                dsp.gate_open_frames, open_after_learning,
                "{reset} dB over {room_db} dBFS: the gate opened"
            );
            assert_eq!(dsp.gain_engine_applied, AGC2);
        }
    }

    /// A tone AGC2 takes for noise (its detector hears no voice), at -65 dBFS,
    /// under the tests' -30 dBFS noise cap: its gain stays where it started.
    fn steady_gain(h: &RootProcessorHarness, rate: i32, position: &mut usize) -> f64 {
        let (mut input, mut output) = (0.0, 0.0);
        for f in 0..60 {
            let frame = tone(rate, -65.0, position);
            let out = h.process(rate, 1, &frame).samples;
            if f >= 30 {
                input += power(&frame);
                output += power(&out);
            }
        }
        10.0 * (output / input).log10()
    }

    /// A gain reset is a new AGC2 starting from it (clamped to AGC2's
    /// maximum); other settings are a new one from the gain applied last;
    /// turning auto gain back on starts over from the last reset.
    #[test]
    fn root_capture_processor_agc2_starts_from_each_gain_reset() {
        let h = agc2_with(48000, 18.0, false, tuning(f32::NAN, -30.0));
        let mut position = 0;
        assert!((steady_gain(&h, 48000, &mut position) - 18.0).abs() < 0.3);
        let dsp = h.dsp(false);
        assert!((dsp.voice_gain_db - 18.0).abs() < 0.3, "{}", dsp.voice_gain_db);
        assert_eq!((dsp.agc2_builds, dsp.gain_engine_applied), (1, AGC2));

        h.gain_reset(6.0);
        assert!((steady_gain(&h, 48000, &mut position) - 6.0).abs() < 0.3);
        assert!(!h.dsp(false).voice_gain_settled);
        h.gain_reset(45.0);
        assert!((steady_gain(&h, 48000, &mut position) - 30.0).abs() < 0.3, "AGC2's 30 dB maximum");
        h.set_agc2_tuning(tuning(12.0, -30.0));
        assert!((steady_gain(&h, 48000, &mut position) - 12.0).abs() < 0.3);
        let dsp = h.dsp(false);
        assert_eq!(
            (dsp.agc2_builds, dsp.agc2_applied.max_gain_db, dsp.agc2_applied.headroom_db),
            (4, 12.0, 5.0)
        );

        h.set_dsp(false, false, 0.0);
        assert!(steady_gain(&h, 48000, &mut position).abs() < 0.01, "auto gain off");
        h.gain_reset(9.0);
        h.set_dsp(true, false, 0.0);
        assert!((steady_gain(&h, 48000, &mut position) - 9.0).abs() < 0.3);
        h.set_gain_engine(0);
        h.gain_reset(4.0);
        h.process(48000, 1, &tone(48000, -65.0, &mut position));
        assert_eq!((h.dsp(false).gain_engine_applied, h.dsp(false).voice_gain_db), (0, 4.0));
        h.reset_controls();
        assert_eq!(h.dsp(false).gain_engine, 0, "a session start forgets the engine");
    }

    /// Not a unit test: a recording through AGC2 in the processor (the gate
    /// open). ROOTAPP_AGC2_REPLAY_IN is a 48 kHz mono float32 WAV of the
    /// Effects SDK's output; ROOTAPP_AGC2_REPLAY_OUT gets the processor's
    /// output (float32 ±1, raw) and `<out>.gain` the gain of each frame (f32
    /// dB). AGC2 starts at ROOTAPP_AGC2_REPLAY_START_DB (15).
    #[test]
    #[ignore]
    fn root_capture_processor_agc2_replays_a_recording() {
        let input = std::env::var("ROOTAPP_AGC2_REPLAY_IN").expect("ROOTAPP_AGC2_REPLAY_IN");
        let output = std::env::var("ROOTAPP_AGC2_REPLAY_OUT").expect("ROOTAPP_AGC2_REPLAY_OUT");
        let start = std::env::var("ROOTAPP_AGC2_REPLAY_START_DB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15.0f32);
        let bytes = std::fs::read(input).unwrap();
        let mut at = 12;
        let mut samples = Vec::new();
        while at + 8 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            if &bytes[at..at + 4] == b"data" {
                samples = bytes[at + 8..(at + 8 + len).min(bytes.len())]
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()) * 32768.0)
                    .collect();
                break;
            }
            at += 8 + len + (len & 1);
        }
        let h = agc2(48000, start, false);
        let mut out = Vec::with_capacity(samples.len() * 4);
        let mut gains = Vec::with_capacity(samples.len() / 120);
        for frame in samples.chunks_exact(480) {
            for s in h.process(48000, 1, frame).samples {
                out.extend_from_slice(&(s / 32768.0).to_le_bytes());
            }
            gains.extend_from_slice(&h.dsp(false).voice_gain_db.to_le_bytes());
        }
        std::fs::write(&output, out).unwrap();
        std::fs::write(format!("{output}.gain"), gains).unwrap();
    }

    /// AGC2 runs at every rate the chain does, from the gain reset, once the
    /// poll built it for the processor's rate; until then (a rate change) the
    /// chain's own gain covers the frames.
    #[test]
    fn root_capture_processor_agc2_runs_at_every_rate_and_follows_a_rate_change() {
        for rate in RATES {
            let h = agc2_with(rate, 12.0, false, tuning(f32::NAN, -30.0));
            let mut position = 0;
            let gain = steady_gain(&h, rate, &mut position);
            assert!((gain - 12.0).abs() < 0.3, "{rate}: {gain}");
            let dsp = h.dsp(false);
            assert_eq!((dsp.agc2_rate, dsp.gain_engine_applied), (rate as u32, AGC2), "{rate}");
        }
        let h = agc2_with(48000, 10.0, false, tuning(f32::NAN, -30.0));
        let mut position = 0;
        steady_gain(&h, 48000, &mut position);
        let builds = h.dsp(false).agc2_builds;
        h.initialize(16000, 1);
        h.process(16000, 1, &tone(16000, -65.0, &mut position));
        assert_eq!(h.dsp(false).gain_engine_applied, 0, "no AGC2 at 16 kHz until the poll");
        h.service_agc2();
        let dsp = h.dsp(false);
        assert_eq!((dsp.agc2_rate, dsp.agc2_builds), (16000, builds + 1));
        let gain = steady_gain(&h, 16000, &mut position);
        assert!((gain - 10.0).abs() < 0.3, "from the gain applied last: {gain}");
        assert_eq!(h.dsp(false).gain_engine_applied, AGC2);
    }
}

#[cfg(test)]
mod speech_tests {
    use super::ffi::{
        new_root_apm_harness, new_root_processor_harness, root_speech_detector_hiss,
        root_speech_detector_probabilities, root_speech_detector_probe,
        root_speech_detector_rate_switch_for_test, root_speech_test_signal, RootProcessorHarness,
    };

    const VOWEL: u32 = 0;
    const WHITE_NOISE: u32 = 1;
    const CLICKS: u32 = 2;
    const TONE: u32 = 3;
    const SILENCE: u32 = 4;
    const BREATH: u32 = 5;
    const KEYS: u32 = 6;
    const ALL: [u32; 7] = [VOWEL, WHITE_NOISE, CLICKS, TONE, SILENCE, BREATH, KEYS];
    const NOISES: [(u32, &str); 3] = [(BREATH, "breath"), (CLICKS, "clicks"), (KEYS, "keys")];

    const OFF: u32 = 0;
    const RING: u32 = 1;
    const GATE: u32 = 2;
    const HELD: u32 = 1;
    /// rootapp_voice_dsp::speech_gate::{CONFIRM_FRAMES, HANG_FRAMES}.
    const CONFIRM_FRAMES: u32 = 3;
    const HANG_FRAMES: usize = 30;
    const HOLD_FRAMES: usize = 20;

    fn frames(rate: i32) -> usize {
        (rate / 100) as usize
    }

    #[test]
    fn root_speech_detector_matches_libwebrtcs_wrapper_at_16_32_and_48_khz() {
        for rate in [16000, 32000, 48000] {
            for signal in ALL {
                let probe = root_speech_detector_probe(signal, rate, 300);
                assert_eq!((probe.frames, probe.unsupported), (300, 0), "{rate}/{signal}");
                assert_eq!(probe.max_diff, 0.0, "{rate}/{signal}: {probe:?}");
            }
        }
    }

    /// Decisions, not exact values: AVX2, SSE2 and NEON round differently.
    #[test]
    fn root_speech_detector_scores_voices_and_rejects_noises() {
        for rate in [16000, 32000, 48000] {
            let vowel = root_speech_detector_probe(VOWEL, rate, 300);
            assert!(vowel.above * 10 >= vowel.judged * 8, "{rate}: {vowel:?}");
            let tone = root_speech_detector_probe(TONE, rate, 300);
            assert!(tone.mean > 0.8, "{rate}: a steady tone counts as speech: {tone:?}");
            let white = root_speech_detector_probe(WHITE_NOISE, rate, 300);
            assert!(white.max < 0.5, "{rate}: {white:?}");
            let silence = root_speech_detector_probe(SILENCE, rate, 300);
            assert_eq!(silence.max, 0.0, "{rate}");
            for (signal, name) in NOISES {
                let probe = root_speech_detector_probe(signal, rate, 600);
                assert!(
                    probe.longest_run < CONFIRM_FRAMES,
                    "{rate} {name}: never confirmed: {probe:?}"
                );
                assert!(probe.mean < 0.3, "{rate} {name}: {probe:?}");
            }
        }
    }

    #[test]
    fn root_speech_detector_judges_a_voice_after_silence_within_30_ms() {
        for rate in [16000, 32000, 48000] {
            let input = script(VOWEL, rate, 40, 30, 0);
            let p = root_speech_detector_probabilities(rate, &input);
            assert_eq!(p.len(), 70);
            assert!(p[..40].iter().all(|&v| v == 0.0), "{rate}: silence");
            let confirmed = (40..70)
                .find(|&f| p[f + 1 - CONFIRM_FRAMES as usize..=f].iter().all(|&v| v >= 0.5))
                .expect("confirmed");
            assert!(confirmed <= 43, "{rate}: confirmed {} ms in: {p:?}", (confirmed - 40) * 10);
        }
    }

    /// Flat white hiss can hold the RNN at speech without AGC2's periodic
    /// reset (97% of 10 min at -60 dBFS, 48 kHz): it starts over after 1 s.
    #[test]
    fn root_speech_detector_starts_over_when_white_hiss_holds_it() {
        let hiss = root_speech_detector_hiss(48000, 60_000);
        assert_eq!(hiss.judged, 60_000);
        assert!(hiss.above * 4 < hiss.judged, "{hiss:?}");
        let vowel = root_speech_detector_probe(VOWEL, 48000, 1000);
        assert!(vowel.above * 10 >= vowel.judged * 8, "a long voice still scores: {vowel:?}");
    }

    #[test]
    fn root_speech_detector_rate_change_drops_the_resamplers_history() {
        assert_eq!(root_speech_detector_rate_switch_for_test(), 0.0);
    }

    #[test]
    fn root_speech_detector_runs_only_at_the_apms_rates() {
        for rate in [8000, 22050, 24000, 44100] {
            let probe = root_speech_detector_probe(VOWEL, rate, 50);
            assert_eq!((probe.unsupported, probe.judged), (50, 0), "{rate}");
        }
        assert_eq!(root_speech_test_signal(VOWEL, 48000, 3).len(), 1440);
        assert!(root_speech_test_signal(7, 48000, 3).is_empty());
    }

    fn harness(rate: i32, mode: u32) -> cxx::UniquePtr<RootProcessorHarness> {
        let h = new_root_processor_harness();
        h.initialize(rate, 1);
        // No voice gain and a manual threshold every fixture is above (-54
        // dBFS): what the level gate alone would send and ring for.
        h.set_dsp(false, false, 0.002);
        h.set_speech_detection(mode);
        h
    }

    /// Runs `signal` frame by frame; returns the published speaking flag and
    /// whether the frame went out.
    fn run(h: &RootProcessorHarness, rate: i32, signal: &[f32]) -> Vec<(bool, bool)> {
        signal
            .chunks(frames(rate))
            .map(|input| {
                let out = h.process(rate, 1, input);
                (h.dsp(false).published_speaking, out.samples.iter().any(|&v| v != 0.0))
            })
            .collect()
    }

    /// `quiet` frames of silence (past the startup hold), `signal` for
    /// `length` frames, then `tail` frames of silence.
    fn script(signal: u32, rate: i32, quiet: u32, length: u32, tail: u32) -> Vec<f32> {
        let mut s = root_speech_test_signal(SILENCE, rate, quiet);
        s.extend(root_speech_test_signal(signal, rate, length));
        s.extend(root_speech_test_signal(SILENCE, rate, tail));
        s
    }

    #[test]
    fn root_capture_processor_speech_gate_rings_breath_clicks_and_keys_only_while_sent() {
        for rate in [16000, 48000] {
            for (signal, name) in NOISES {
                let input = script(signal, rate, 30, 400, 0);
                let level = run(&harness(rate, OFF), rate, &input);
                assert!(level.iter().any(|&(ring, _)| ring), "{rate} {name}: the level gate rings");

                let h = harness(rate, GATE);
                let gate = run(&h, rate, &input);
                assert!(
                    gate.iter().all(|&(ring, sent)| ring || !sent),
                    "{rate} {name}: what is sent rings"
                );
                let dsp = h.dsp(true);
                assert_eq!(dsp.speech_frames, 0, "{rate} {name}");
                assert!(dsp.speech_detector_running && !dsp.speech, "{rate} {name}");
                assert_eq!(dsp.speech_unavailable_frames, 0, "{rate} {name}");
                // The breath holds the level gate open, so the speech gate
                // closes it after the 200 ms provisional open and keeps it shut
                // (above the threshold, under 0.5). A click or a key opens the
                // level gate for less than that, as without the detector.
                if signal == BREATH {
                    assert!(dsp.speech_closed_frames > 0, "{rate} {name}");
                    let sent = |frames: &[(bool, bool)]| frames.iter().filter(|f| f.1).count();
                    assert!(
                        sent(&gate) * 2 < sent(&level),
                        "{rate} {name}: {} of {} frames sent",
                        sent(&gate),
                        sent(&level)
                    );
                }
            }
        }
    }

    #[test]
    fn root_capture_processor_speech_gate_rings_within_60_ms_and_300_ms_after_a_voice() {
        for rate in [16000, 32000, 48000] {
            let (quiet, voice) = (40usize, 150usize);
            let input = script(VOWEL, rate, quiet as u32, voice as u32, 120);
            let level = run(&harness(rate, OFF), rate, &input);
            let h = harness(rate, GATE);
            let gate = run(&h, rate, &input);

            let onset = (0..gate.len()).find(|&f| gate[f].0).expect("the voice rings");
            assert!(
                (quiet..=quiet + 6).contains(&onset),
                "{rate}: rings {} ms after the voice starts",
                (onset as i64 - quiet as i64) * 10
            );
            assert!(
                (0..onset).all(|f| !gate[f].1) && gate[onset].1,
                "{rate}: nothing goes out before the detector confirms, and the confirmation opens it"
            );

            let end = quiet + voice;
            let ring_off = |frames: &[(bool, bool)]| (end..frames.len()).find(|&f| !frames[f].0);
            let gate_off = ring_off(&gate).expect("the ring ends") - end;
            let level_off = ring_off(&level).expect("the level ring ends") - end;
            assert!(
                (HANG_FRAMES - 3..=HANG_FRAMES + 8).contains(&gate_off),
                "{rate}: ring ends {} ms after the voice",
                gate_off * 10
            );
            assert!(level_off >= 65, "{rate}: the level gate alone rings {} ms", level_off * 10);
            let dsp = h.dsp(true);
            assert!(dsp.speech_frames >= (voice as u64) * 8 / 10, "{rate}: {dsp:?}");
            assert!(dsp.speech_probability_max > 0.9, "{rate}");
            assert_eq!(dsp.speech_detection, GATE);
        }
    }

    #[test]
    fn root_capture_processor_ring_mode_leaves_the_audio_to_the_level_gate() {
        let input = script(CLICKS, 48000, 30, 200, 0);
        let mut input = input;
        input.extend(root_speech_test_signal(VOWEL, 48000, 100));
        let off = harness(48000, OFF);
        let ring = harness(48000, RING);
        let mut rang = false;
        for chunk in input.chunks(480) {
            let a = off.process(48000, 1, chunk);
            let b = ring.process(48000, 1, chunk);
            assert_eq!(a.samples, b.samples);
            let (off_dsp, ring_dsp) = (off.dsp(false), ring.dsp(false));
            assert!(!ring_dsp.published_speaking || off_dsp.published_speaking);
            rang |= ring_dsp.published_speaking;
        }
        assert!(rang, "the voice rings");
        let (off_dsp, ring_dsp) = (off.dsp(true), ring.dsp(true));
        assert_eq!(ring_dsp.speech_closed_frames, 0);
        assert!(ring_dsp.speech_frames > 50);
        assert!(!off_dsp.speech_detector_running && off_dsp.speech_probability_max.is_nan());
        assert_eq!((off_dsp.speech_frames, off_dsp.speech_unavailable_frames), (0, 0));
    }

    #[test]
    fn root_capture_processor_speech_detection_needs_a_rate_the_detector_runs_at() {
        for rate in [8000, 44100] {
            let input = script(CLICKS, rate, 30, 100, 0);
            let off = run(&harness(rate, OFF), rate, &input);
            let h = harness(rate, GATE);
            let gate = run(&h, rate, &input);
            assert_eq!(off, gate, "{rate}: the level gate alone, bit for bit");
            let dsp = h.dsp(true);
            assert_eq!(dsp.speech_unavailable_frames, 130, "{rate}");
            assert!(!dsp.speech_detector_running && dsp.speech_probability_max.is_nan());
        }
    }

    #[test]
    fn root_capture_processor_push_to_talk_and_mute_bypass_the_speech_gate() {
        let input = script(WHITE_NOISE, 48000, 30, 100, 0);
        let h = harness(48000, GATE);
        h.set_push_to_talk(HELD);
        let held = run(&h, 48000, &input);
        assert!(held[30..].iter().all(|&(ring, sent)| ring && sent), "the key decides");
        assert!(held[HOLD_FRAMES + 2..30].iter().all(|&(ring, _)| ring));
        let dsp = h.dsp(true);
        assert!(
            dsp.speech_detector_running && dsp.speech_probability_max >= 0.0,
            "the verdict keeps running under push-to-talk"
        );
        assert_eq!((dsp.speech_closed_frames, dsp.speech_unavailable_frames), (0, 0));

        let h = harness(48000, GATE);
        h.set_mic_muted(true);
        let mut input = script(SILENCE, 48000, 30, 0, 0);
        input.extend(root_speech_test_signal(VOWEL, 48000, 60));
        let muted = run(&h, 48000, &input);
        assert!(muted.iter().all(|&(ring, sent)| !ring && !sent));
        assert!(h.dsp(false).speech, "the detector keeps judging while muted");
        h.set_mic_muted(false);
        let vowel = root_speech_test_signal(VOWEL, 48000, 61);
        run(&h, 48000, &vowel[vowel.len() - 480..]);
        assert!(h.dsp(false).published_speaking, "unmuting mid-word rings at once");
    }

    #[test]
    fn root_capture_processor_a_reset_turns_speech_detection_off() {
        let h = harness(48000, GATE);
        run(&h, 48000, &script(VOWEL, 48000, 30, 10, 0));
        assert_eq!(h.dsp(false).speech_detection, GATE);
        let before = h.dsp(true);
        h.reset_controls();
        h.set_dsp(false, false, 0.002);
        let input = script(CLICKS, 48000, 30, 100, 0);
        let after = run(&h, 48000, &input);
        let dsp = h.dsp(true);
        assert_eq!(dsp.speech_detection, OFF);
        assert!(!dsp.speech_detector_running && dsp.speech_probability_max.is_nan());
        assert_eq!(dsp.speech_closed_frames, before.speech_closed_frames);
        assert_eq!(after, run(&harness(48000, OFF), 48000, &input), "the level gate alone");
    }

    #[test]
    fn root_apm_decorator_real_apm_runs_the_speech_detector_at_the_processors_rate() {
        // A steady tone counts as speech. 8 kHz streams reach it at 16 kHz.
        for (rate, processed) in [(48000, 48000), (32000, 32000), (16000, 16000), (8000, 16000)] {
            let h = new_root_apm_harness(true);
            h.engine_apply(true, false, false, false, true);
            h.set_dsp(false, false, 0.003);
            h.set_speech_detection(GATE);
            for _ in 0..60 {
                h.process_frame(rate, 1, 8000);
            }
            assert_eq!(h.processor_rate(), processed, "{rate}");
            let dsp = h.dsp(true);
            assert!(dsp.speech_detector_running && dsp.speech, "{rate}: {dsp:?}");
            assert_eq!(dsp.speech_unavailable_frames, 0, "{rate}");
            assert!(h.speaking(), "{rate}");
        }
    }
}

#[cfg(test)]
mod decorator_tests {
    use super::ffi::{
        new_root_apm_harness, root_apm_decorator_race_for_test,
        root_apm_policy_stats_never_wait_for_an_apply_for_test, RootApmHarness,
    };

    const KEEP: i8 = -1;

    fn fake() -> cxx::UniquePtr<RootApmHarness> {
        new_root_apm_harness(false)
    }

    fn threshold_db(h: &RootApmHarness) -> f64 {
        20.0 * f64::from(h.dsp(false).threshold).log10()
    }

    /// The real APM, no echo canceller, auto sensitivity on and no voice gain.
    fn real_auto() -> cxx::UniquePtr<RootApmHarness> {
        let h = new_root_apm_harness(true);
        h.engine_apply(false, false, false, false, true);
        h.set_dsp(false, true, 0.025);
        h
    }

    /// A hardware mute through the real APM: the decorator flags raw int16 frames of zeros (at any
    /// channel count) before the APM, so auto sensitivity keeps what the room taught it whatever the
    /// high-pass filter's tail; one int16 step of capture is live and is learned.
    #[test]
    fn root_apm_decorator_flags_digital_silence_on_the_raw_capture() {
        for channels in [1usize, 2] {
            let h = real_auto();
            for _ in 0..500 {
                h.process_frame(48000, channels, 73);
            }
            let learned = h.dsp(false).threshold;
            assert!((threshold_db(&h) + 48.0).abs() < 1.0, "{channels}: {}", threshold_db(&h));
            for _ in 0..300 {
                h.process_frame(48000, channels, 0);
            }
            assert_eq!(h.dsp(false).threshold, learned, "{channels}");
            for _ in 0..300 {
                h.process_frame(48000, channels, 1);
            }
            assert!(threshold_db(&h) < -53.0, "{channels}: {}", threshold_db(&h));
        }
    }

    /// The float path's bound: a frame peaking at -100 dBFS is silence, one at -94 dBFS is not.
    #[test]
    fn root_apm_decorator_float_capture_is_silent_up_to_minus_100_dbfs() {
        let room = 10f32.powf(-56.0 / 20.0) * std::f32::consts::SQRT_2;
        for (amplitude, silent) in [(0.0f32, true), (1e-5, true), (2e-5, false)] {
            let h = real_auto();
            for _ in 0..500 {
                h.process_float_frame(48000, 1, room);
            }
            let learned = h.dsp(false).threshold;
            assert!((threshold_db(&h) + 48.0).abs() < 1.0, "{}", threshold_db(&h));
            for _ in 0..300 {
                h.process_float_frame(48000, 1, amplitude);
            }
            if silent {
                assert_eq!(h.dsp(false).threshold, learned, "{amplitude}");
            } else {
                assert!(threshold_db(&h) < -53.0, "{amplitude}: {}", threshold_db(&h));
            }
        }
    }

    #[test]
    fn root_apm_decorator_never_enables_agc_and_keeps_the_high_pass_filter() {
        let h = fake();
        for (aec, ns, agc1, agc2, hpf) in [
            (true, true, true, true, false),
            (true, false, true, false, false),
            (false, true, false, true, true),
        ] {
            h.engine_apply(aec, ns, agc1, agc2, hpf);
            let probe = h.probe();
            assert!(!probe.inner_agc1 && !probe.inner_agc2, "{probe:?}");
            assert!(probe.inner_hpf);
            assert_eq!((probe.inner_aec, probe.inner_ns), (aec, ns));
            assert_eq!((probe.aec_active, probe.ns_active), (aec, ns));
            assert_eq!((probe.served_aec, probe.served_ns), (aec, ns));
            assert!(!probe.served_agc && probe.served_hpf);
        }
    }

    #[test]
    fn root_apm_decorator_never_copies_the_policy_into_the_engines_request() {
        let h = fake();
        h.engine_apply(true, false, false, false, true);
        h.set_policy(KEEP, 1);
        h.run_due_tasks();
        let probe = h.probe();
        assert!(probe.inner_ns && !probe.served_ns, "{probe:?}");

        h.engine_apply_echo_cancellation(false);
        assert!(h.probe().inner_ns);
        h.advance_clock(1000);
        h.reset_policy();
        h.run_due_tasks();
        let probe = h.probe();
        assert!(!probe.inner_ns && !probe.ns_active, "the engine never asked for NS");
        assert!(!probe.inner_aec);
    }

    #[test]
    fn root_apm_decorator_survives_engine_applies_racing_policy_changes() {
        assert!(root_apm_decorator_race_for_test(5000));
    }

    #[test]
    fn root_apm_decorator_policy_stats_never_wait_for_an_apm_reinit() {
        assert!(root_apm_policy_stats_never_wait_for_an_apply_for_test());
    }

    #[test]
    fn root_apm_decorator_applies_only_a_changed_config() {
        let h = fake();
        h.engine_apply(true, true, true, false, true);
        let applies = h.probe().fake_applies;
        h.engine_apply(true, true, true, false, true);
        h.engine_apply(true, true, false, false, false);
        assert_eq!(h.probe().fake_applies, applies);
        h.engine_apply(false, true, false, false, true);
        assert_eq!(h.probe().fake_applies, applies + 1);
        assert_eq!(h.probe().inner_applies, h.probe().fake_applies);
    }

    #[test]
    fn root_apm_decorator_swallows_mute_so_capture_output_stays_used() {
        let h = fake();
        h.set_output_will_be_muted(true);
        assert!(h.post_capture_output_used(false));
        h.set_output_will_be_muted(false);
        let probe = h.probe();
        assert_eq!((probe.fake_mute_calls, probe.fake_output_used_settings), (0, 0));
        assert_eq!(probe.swallowed, 4);
        assert!(!probe.output_will_be_muted);
    }

    #[test]
    fn root_apm_decorator_policy_overrides_the_engine() {
        let h = fake();
        h.engine_apply(true, true, false, false, true);
        h.set_policy(0, KEEP);
        assert_eq!(h.run_due_tasks(), 1);
        let probe = h.probe();
        assert!(!probe.inner_aec && probe.inner_ns);
        assert!(probe.served_aec, "the engine keeps seeing its own request");

        h.engine_apply(true, false, false, false, true);
        let probe = h.probe();
        assert!(!probe.inner_aec && !probe.inner_ns);
        assert!(!probe.aec_active);

        h.advance_clock(1000);
        h.reset_policy();
        assert_eq!(h.run_due_tasks(), 1);
        h.engine_apply(true, false, false, false, true);
        assert!(h.probe().inner_aec);
    }

    #[test]
    fn root_apm_decorator_applies_policy_at_most_once_a_second_and_coalesces() {
        let h = fake();
        h.engine_apply(true, true, false, false, true);
        h.set_policy(0, KEEP);
        assert_eq!(h.run_due_tasks(), 1);
        let applies = h.probe().fake_applies;

        h.set_policy(1, KEEP);
        h.set_policy(0, KEEP);
        h.set_policy(1, 0);
        assert_eq!(h.pending_tasks(), 1);
        assert_eq!(h.run_due_tasks(), 0);
        h.advance_clock(999);
        assert_eq!(h.run_due_tasks(), 0);
        assert!(!h.probe().inner_aec);

        h.advance_clock(1);
        assert_eq!(h.run_due_tasks(), 1);
        let probe = h.probe();
        assert!(probe.inner_aec && !probe.inner_ns);
        assert_eq!(probe.fake_applies, applies + 1);
    }

    #[test]
    fn root_apm_decorator_holds_a_pending_policy_back_from_engine_applies() {
        let h = fake();
        h.engine_apply(true, true, false, false, true);
        h.set_policy(0, KEEP);
        h.run_due_tasks();
        h.set_policy(1, KEEP);
        h.engine_apply(false, true, false, false, true);
        assert!(!h.probe().inner_aec);
        h.advance_clock(1000);
        h.run_due_tasks();
        assert!(h.probe().inner_aec);
    }

    #[test]
    fn root_apm_decorator_policy_matching_the_current_one_schedules_nothing() {
        let h = fake();
        h.set_policy(KEEP, KEEP);
        assert_eq!(h.pending_tasks(), 0);
        h.reset_policy();
        assert_eq!(h.pending_tasks(), 0);
    }

    #[test]
    fn root_apm_decorator_runs_the_capture_processor_every_frame_on_a_real_apm() {
        let h = new_root_apm_harness(true);
        h.engine_apply(true, false, true, true, true);
        h.set_output_will_be_muted(true);
        h.post_capture_output_used(false);
        // The startup hold, then five gate blocks in auto sensitivity.
        for _ in 0..25 {
            h.process_frame(48000, 1, 8000);
        }
        assert_eq!(h.processed_frames(), 25);
        let probe = h.probe();
        assert!(probe.inner_aec && !probe.inner_agc1 && !probe.inner_agc2 && probe.inner_hpf);
        assert!(!probe.output_will_be_muted);
        assert!(h.process_frame(48000, 1, 8000) > 1000);
        assert!(h.speaking());

        h.set_mic_muted(true);
        h.process_frame(48000, 1, 8000);
        assert!(!h.speaking());
        assert_eq!(h.process_frame(48000, 1, 8000), 0, "after the 5 ms fade");
        assert_eq!(h.processed_frames(), 28);
    }

    #[test]
    fn root_apm_decorator_real_apm_feeds_the_processor_at_the_stream_rate() {
        // (stream rate, channels, rate the processor sees): the APM never
        // processes below 16 kHz, and a 48 kHz stream reaches the post-processor
        // through its full-band buffer with every output channel.
        for (rate, channels, processed_rate) in
            [(16000, 1, 16000), (8000, 1, 16000), (48000, 2, 48000), (32000, 1, 32000)]
        {
            let h = new_root_apm_harness(true);
            h.engine_apply(true, false, false, false, true);
            h.set_dsp(false, false, 0.0);
            for _ in 0..25 {
                h.process_frame(rate, channels, 8000);
            }
            assert_eq!(h.processed_frames(), 25, "{rate}/{channels}");
            assert_eq!(h.processor_rate(), processed_rate, "{rate}/{channels}");
            assert!(h.processor_channels() >= 1, "{rate}/{channels}");
            assert!(h.process_frame(rate, channels, 8000) > 1000, "{rate}/{channels}");
            h.set_mic_muted(true);
            // The gate fades over the first 5 ms of the first muted frame (and at
            // 8 kHz the APM's resampler still holds unmuted samples for it).
            h.process_frame(rate, channels, 8000);
            assert_eq!(h.process_frame(rate, channels, 8000), 0, "{rate}/{channels}");
        }
    }

    #[test]
    fn root_apm_decorator_real_apm_runs_the_voice_chain_at_48_and_16_khz() {
        for rate in [48000, 16000] {
            let h = new_root_apm_harness(true);
            h.engine_apply(true, false, false, false, true);
            for _ in 0..20 {
                assert_eq!(h.process_frame(rate, 1, 8000), 0, "{rate}: the startup hold");
            }
            assert!(!h.speaking());
            let mut loudest = 0;
            for _ in 0..20 {
                loudest = loudest.max(h.process_frame(rate, 1, 8000));
            }
            assert!(loudest > 1000, "{rate}: {loudest}");
            assert!(h.speaking(), "{rate}");
            for _ in 0..80 {
                h.process_frame(rate, 1, 0);
            }
            assert!(!h.speaking(), "{rate}: released after the gate's hold");
        }
    }

    #[test]
    fn root_apm_decorator_waits_for_a_pause_in_speech() {
        let h = fake();
        h.engine_apply(true, false, false, false, true);
        h.set_speaking(true);
        h.set_policy(KEEP, 1);
        assert_eq!(h.run_due_tasks(), 1);
        assert!(!h.probe().inner_ns, "deferred while speaking");
        assert!(h.policy().pending);
        h.advance_clock(50);
        assert_eq!(h.run_due_tasks(), 1);
        assert!(!h.probe().inner_ns);
        h.set_speaking(false);
        h.advance_clock(50);
        assert_eq!(h.run_due_tasks(), 1);
        assert!(h.probe().inner_ns);
        let policy = h.policy();
        assert_eq!((policy.applies, policy.deferred, policy.forced), (1, 1, 0));
        assert!(!policy.pending && policy.webrtc_ns_applied);
        assert_eq!(h.pending_tasks(), 0);
    }

    #[test]
    fn root_apm_decorator_applies_at_the_3_s_limit_while_speaking() {
        let h = fake();
        h.engine_apply(true, false, false, false, true);
        h.set_speaking(true);
        h.set_policy(0, KEEP);
        let mut waited = 0;
        loop {
            h.run_due_tasks();
            if !h.probe().inner_aec {
                break;
            }
            h.advance_clock(50);
            waited += 50;
            assert!(waited <= 3000);
        }
        assert_eq!(waited, 3000);
        let policy = h.policy();
        assert_eq!((policy.applies, policy.deferred, policy.forced), (1, 1, 1));
        assert!(!policy.pending);
    }

    #[test]
    fn root_apm_decorator_fallback_turns_ns_on_at_once() {
        let h = fake();
        h.engine_apply(true, false, false, false, true);
        h.set_policy(1, 0);
        assert_eq!(h.run_due_tasks(), 1);
        h.advance_clock(100);
        h.set_speaking(true);
        h.set_policy(0, KEEP);
        h.set_fallback_ns(true);
        assert_eq!(h.run_due_tasks(), 1, "inside the rate-limit window, while speaking");
        let probe = h.probe();
        assert!(probe.inner_ns && probe.ns_active);
        assert!(!probe.inner_aec, "one application carries every pending change");
        let policy = h.policy();
        assert!(policy.fallback_ns && policy.webrtc_ns_applied && !policy.pending);
        assert_eq!((policy.applies, policy.deferred, policy.forced), (2, 0, 0));

        h.advance_clock(1000);
        h.run_due_tasks();
        assert_eq!(h.policy().applies, 2, "the replaced application does nothing");
        h.engine_apply(true, false, false, false, true);
        assert!(h.probe().inner_ns, "the engine cannot turn the fallback off");
    }

    #[test]
    fn root_apm_decorator_fallback_off_waits_for_the_rate_limit_and_a_pause() {
        let h = fake();
        h.engine_apply(true, false, false, false, true);
        h.set_fallback_ns(true);
        assert_eq!(h.run_due_tasks(), 1);
        assert!(h.probe().inner_ns);
        h.advance_clock(200);
        h.set_speaking(true);
        h.set_fallback_ns(false);
        assert!(h.policy().pending);
        h.advance_clock(799);
        assert_eq!(h.run_due_tasks(), 0);
        h.advance_clock(1);
        assert_eq!(h.run_due_tasks(), 1);
        assert!(h.probe().inner_ns, "deferred while speaking");
        h.set_speaking(false);
        h.advance_clock(50);
        assert_eq!(h.run_due_tasks(), 1);
        let probe = h.probe();
        assert!(!probe.inner_ns && !probe.ns_active);
        let policy = h.policy();
        assert!(!policy.fallback_ns && !policy.webrtc_ns_applied && !policy.pending);
        assert_eq!((policy.applies, policy.deferred), (2, 1));
    }

    #[test]
    fn root_apm_decorator_reset_policy_clears_the_fallback() {
        let h = fake();
        h.engine_apply(true, true, false, false, true);
        assert!(h.policy().webrtc_ns_applied, "the engine's own NS counts");
        h.engine_apply(true, false, false, false, true);
        assert!(!h.policy().webrtc_ns_applied);
        h.set_fallback_ns(true);
        h.run_due_tasks();
        assert!(h.policy().webrtc_ns_applied);
        h.advance_clock(1000);
        h.reset_policy();
        h.run_due_tasks();
        let policy = h.policy();
        assert!(!policy.fallback_ns && !policy.webrtc_ns_applied && !policy.pending);
        h.set_fallback_ns(false);
        assert_eq!(h.pending_tasks(), 0, "already off");
    }
}
