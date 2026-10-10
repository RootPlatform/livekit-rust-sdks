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
    DenoiserOutput, DspChain, DspCommands, DspOutputs, DspSettings, PushToTalk,
};

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    /// `rootapp_voice_dsp::DspSettings`; push_to_talk 0 voice activity, 1 held,
    /// anything else released.
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
    }

    extern "Rust" {
        type RootDspChain;

        fn new_root_dsp_chain(max_rate: u32) -> Box<RootDspChain>;
        fn pre_denoise(
            self: &mut RootDspChain,
            frame: &mut [f32],
            settings: &RootDspSettings,
            commands: &RootDspCommands,
            outputs: &mut RootDspOutputs,
        ) -> RootDspPre;
        /// kind: 0 not in use, 1 missed, 2 ran (denoised and restart are used
        /// only for 2).
        fn post_denoise(
            self: &mut RootDspChain,
            frame: &mut [f32],
            kind: u8,
            denoised: &[f32],
            restart: bool,
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
        fn set_webrtc_ns_applied(self: &RootProcessorHarness, applied: bool);
        /// As AdmProxy does before each StartRecording.
        fn bump_stream_epoch(self: &RootProcessorHarness);
        /// As the controller does: payload, then the sequence number.
        fn gain_reset(self: &RootProcessorHarness, db: f32);
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
        /// Stores the speaking flag as the processor publishes it.
        fn set_published_speaking(self: &RootProcessorHarness, speaking: bool);
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
        fn set_mic_muted(self: &RootApmHarness, muted: bool);
        fn set_dsp(
            self: &RootApmHarness,
            auto_gain: bool,
            auto_sensitivity: bool,
            gate_threshold: f32,
        );
        fn processed_frames(self: &RootApmHarness) -> u64;
        fn processor_rate(self: &RootApmHarness) -> u32;
        fn processor_channels(self: &RootApmHarness) -> u32;
        /// The capture state's published speaking flag.
        fn speaking(self: &RootApmHarness) -> bool;
        /// Overrides the published speaking flag the decorator waits on.
        fn set_speaking(self: &RootApmHarness, speaking: bool);
        fn set_fallback_ns(self: &RootApmHarness, on: bool);
        fn policy(self: &RootApmHarness) -> RootPolicyProbe;

        /// The engine's ApplyConfig racing policy changes on another thread.
        fn root_apm_decorator_race_for_test(iterations: u32) -> bool;
        /// A policy stats read during another thread's inner ApplyConfig
        /// returns at once.
        fn root_apm_policy_stats_never_wait_for_an_apply_for_test() -> bool;
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

    fn post_denoise(
        &mut self,
        frame: &mut [f32],
        kind: u8,
        denoised: &[f32],
        restart: bool,
        settings: &ffi::RootDspSettings,
        outputs: &mut ffi::RootDspOutputs,
    ) {
        let denoised = match kind {
            1 => DenoiserOutput::Missed,
            2 => DenoiserOutput::Ran { samples: denoised, restart },
            _ => DenoiserOutput::NotInUse,
        };
        self.chain.post_denoise(frame, denoised, &dsp_settings(settings), &mut self.out);
        copy_outputs(&self.out, outputs);
    }
}

#[cfg(test)]
mod processor_tests {
    use super::ffi::{
        new_root_processor_harness, new_root_processor_harness_without_slot, RootProcessorHarness,
    };

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
        h.gain_reset(12.0);
        run(&h, 48000, 1, 0.0);
        let dsp = h.dsp(false);
        assert_eq!(dsp.voice_gain_db, 12.0);
        assert!(!dsp.voice_gain_settled);

        h.gain_reset(30.0);
        run(&h, 48000, 1, 0.0);
        assert_eq!(h.dsp(false).voice_gain_db, 18.0, "clamped to the voice gain's range");
        h.gain_reset(f32::NAN);
        run(&h, 48000, 1, 0.0);
        assert_eq!(h.dsp(false).voice_gain_db, 18.0, "a non-finite reset is ignored");

        h.set_dsp(false, true, 0.025);
        run(&h, 48000, 1, 0.0);
        assert_eq!(h.dsp(false).voice_gain_db, 18.0, "off keeps the last value");
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
