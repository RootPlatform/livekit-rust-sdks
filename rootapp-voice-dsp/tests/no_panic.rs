#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects,
    clippy::manual_is_multiple_of
)]

mod support;

use rootapp_voice_dsp::{
    Agc2Tuning, DenoiserOutput, DspChain, DspCommands, DspOutputs, DspSettings, GainEngine,
    PushToTalk, SpeechDetection, VoiceGainTuning,
};
use support::Rng;

const RATES: [u32; 13] =
    [0, 1, 7999, 8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 48001, 96000];
const FLOATS: [f32; 9] =
    [1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 1e30, -1e30, 0.0, 2.5];

fn lengths(rate: u32) -> [usize; 8] {
    let n = (rate / 100) as usize;
    [0, 1, n.saturating_sub(1), n, n + 1, 480, 481, 960]
}

fn fill(frame: &mut [f32], kind: usize, rng: &mut Rng) {
    for s in frame.iter_mut() {
        *s = match kind {
            0 => 0.0,
            1 => f32::NAN,
            2 => f32::INFINITY,
            3 => f32::NEG_INFINITY,
            4 => 1e-40,
            5 => 1e30,
            6 => -1e30,
            7 => 40_000.0 * rng.symmetric(),
            _ => {
                let v = rng.next_u64();
                [f32::NAN, 1e30, -1e30, 1e-40, 32767.0, -32768.0, 0.0][(v % 7) as usize]
            }
        };
    }
}

/// Voice gain tunables from FLOATS (NaN, infinities, negative, huge, 0) and the bounds.
fn tuning(step: usize) -> VoiceGainTuning {
    let f = |k: usize, scale: f32| FLOATS[(step / 7 + k) % FLOATS.len()] * scale;
    match step % 4 {
        0 => VoiceGainTuning::MIN,
        1 => VoiceGainTuning::MAX,
        _ => VoiceGainTuning {
            max_gain_db: f(0, 20.0),
            target_db: f(1, -30.0),
            rise_db_per_second: f(2, 5.0),
            fall_db_per_second: f(3, 5.0),
            level_time_constant_ms: f(4, 3000.0),
            min_speech_probability: f(5, 0.5),
        },
    }
}

/// AGC2 settings from FLOATS and the bounds.
fn agc2(step: usize) -> Agc2Tuning {
    let f = |k: usize, scale: f32| FLOATS[(step / 5 + k) % FLOATS.len()] * scale;
    match step % 3 {
        0 => Agc2Tuning::MIN,
        1 => Agc2Tuning::MAX,
        _ => Agc2Tuning {
            headroom_db: f(0, 5.0),
            max_gain_db: f(1, 30.0),
            max_gain_change_db_per_second: f(2, 6.0),
            max_output_noise_dbfs: f(3, -50.0),
        },
    }
}

fn denoiser_output(kind: usize, n: usize) -> usize {
    match kind {
        0 => 0,
        1 => 1,
        2 => 8 * n,
        _ => 8 * n + 500,
    }
}

fn check(frame: &[f32], out: &DspOutputs, context: &str) {
    for (i, &s) in frame.iter().enumerate() {
        assert!(s.is_finite() && s.abs() <= 32768.0, "{context}: sample {i} = {s}");
    }
    for (name, v) in [
        ("apm_out_sumsq", out.apm_out_sumsq),
        ("denoised_sumsq", out.denoised_sumsq),
        ("gate_in_sumsq", out.gate_in_sumsq),
        ("sent_sumsq", out.sent_sumsq),
    ] {
        assert!(v.is_finite() && v >= 0.0, "{context}: {name} = {v}");
    }
    for (name, v) in [
        ("gate_in_rms", out.gate_in_rms),
        ("threshold", out.threshold),
        ("envelope", out.envelope),
        ("voice_gain_db", out.voice_gain_db),
        ("latency_ms", out.aligner.latency_ms),
        ("lifetime_latency_ms", out.aligner.lifetime_latency_ms),
    ] {
        assert!(v.is_finite(), "{context}: {name} = {v}");
    }
    assert!(!out.gate_in_block_peak_db.is_nan(), "{context}");
    assert!((0.0..=1.0).contains(&out.envelope), "{context}");
    assert!((0.0..=1.0).contains(&out.threshold), "{context}: {}", out.threshold);
    assert!(
        (0.0..=Agc2Tuning::MAX.max_gain_db).contains(&out.voice_gain_db),
        "{context}: {}",
        out.voice_gain_db
    );
    assert!(!out.speaking || out.transmit_open, "{context}");
}

/// Rates × lengths × inputs × denoiser outputs × push-to-talk values, with settings and commands
/// cycling through NaN, infinities, negative and huge values, on one chain (so every case also
/// follows an abrupt change) and on fresh ones.
#[test]
fn any_shape_settings_input_and_denoiser_output_leaves_finite_full_scale_audio() {
    let mut rng = Rng::new(21);
    let mut chain = DspChain::new(48_000);
    let mut out = DspOutputs::default();
    let mut denoised = vec![0.0f32; 8 * 960 + 500];
    let mut frame = vec![0.0f32; 1024];
    let mut step = 0usize;
    for rate in RATES {
        for len in lengths(rate) {
            for input_kind in 0..9 {
                for denoiser_kind in 0..5 {
                    for ptt in 0..6u32 {
                        step += 1;
                        let settings = DspSettings {
                            sample_rate: rate,
                            mic_muted: step % 7 == 0,
                            push_to_talk: PushToTalk::from_wire(ptt),
                            input_volume: FLOATS[step % FLOATS.len()],
                            auto_gain: step % 3 != 0,
                            gate_threshold: FLOATS[(step / 3) % FLOATS.len()],
                            auto_sensitivity: step % 5 < 3,
                            denoiser_wanted: step % 4 < 2,
                            speech_detection: SpeechDetection::from_wire((step % 3) as u32),
                            speech_probability: [f32::NAN, 0.9, -1.0, 2.0, f32::INFINITY, 0.2]
                                [step % 6],
                            capture_silent: step % 8 < 3,
                            auto_sensitivity_minimum_db: FLOATS[(step / 5) % FLOATS.len()] * -50.0,
                            voice_gain: tuning(step),
                            gain_engine: GainEngine::from_wire((step % 3) as u32),
                            agc2: agc2(step),
                        };
                        let commands = DspCommands {
                            reset: step % 29 == 0,
                            flush: step % 13 == 0,
                            gain_reset: step % 11 == 0,
                            gain_reset_db: FLOATS[(step / 11) % FLOATS.len()] * 10.0,
                        };
                        let frame = &mut frame[..len];
                        fill(frame, input_kind, &mut rng);
                        let m = denoiser_output(denoiser_kind, (rate / 100) as usize)
                            .min(denoised.len());
                        if denoiser_kind == 4 {
                            denoised[..m].fill(f32::NAN);
                        } else {
                            fill(&mut denoised[..m], 7, &mut rng);
                        }
                        let output = match step % 4 {
                            0 => DenoiserOutput::NotInUse,
                            1 => DenoiserOutput::Missed,
                            _ => DenoiserOutput::Ran {
                                samples: &denoised[..m],
                                restart: step % 9 == 0,
                            },
                        };
                        let context = format!("rate {rate} len {len} input {input_kind} denoiser {denoiser_kind} ptt {ptt}");
                        chain.pre_denoise(frame, &settings, &commands, &mut out);
                        chain.post_denoise(frame, output, &settings, &mut out);
                        check(frame, &out, &context);

                        if step % 37 == 0 {
                            let mut fresh = DspChain::new(rate);
                            let mut fresh_out = DspOutputs::default();
                            fill(frame, input_kind, &mut rng);
                            fresh.process(frame, &settings, &commands, &mut fresh_out);
                            check(frame, &fresh_out, &context);
                        }
                    }
                }
            }
        }
    }
}

/// Abrupt rate and length changes every frame, commands in every state, wrong-length frames
/// carrying pending commands, post without pre, and long runs of loud input.
#[test]
fn abrupt_changes_and_commands_in_every_state_never_panic() {
    let rates = [8000u32, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 96000, 1];
    let mut rng = Rng::new(5);
    let mut chain = DspChain::new(48_000);
    let mut out = DspOutputs::default();
    let mut frame = vec![0.0f32; 960];
    let denoised: Vec<f32> = (0..8 * 480 + 500).map(|_| 60_000.0 * rng.symmetric()).collect();
    for step in 0..60_000usize {
        let v = rng.next_u64();
        let rate = if v % 5 == 0 { rates[(v / 5 % rates.len() as u64) as usize] } else { 48000 };
        let n = (rate / 100) as usize;
        let len = match (v >> 8) % 8 {
            0 => (v >> 16) as usize % 961,
            1 => n.saturating_sub(1),
            _ => n.min(960),
        };
        let settings = DspSettings {
            sample_rate: rate,
            mic_muted: (v >> 20) % 9 == 0,
            push_to_talk: PushToTalk::from_wire(((v >> 24) % 4) as u32),
            input_volume: [1.0, 2.0, 0.0, f32::NAN][((v >> 28) % 4) as usize],
            auto_gain: (v >> 30) % 4 != 0,
            gate_threshold: [0.025, 0.0, 1.0, 0.5][((v >> 32) % 4) as usize],
            auto_sensitivity: (v >> 34) % 3 != 0,
            denoiser_wanted: (v >> 36) % 2 == 0,
            speech_detection: SpeechDetection::from_wire(((v >> 38) % 3) as u32),
            speech_probability: [f32::NAN, 0.95, 0.05, -0.5, 1.5][((v >> 40) % 5) as usize],
            capture_silent: (v >> 42) % 3 == 0,
            auto_sensitivity_minimum_db: [-54.0, f32::NAN, f32::NEG_INFINITY, 20.0, -1e30]
                [((v >> 46) % 5) as usize],
            voice_gain: tuning((v >> 48) as usize),
            gain_engine: GainEngine::from_wire(((v >> 18) % 2) as u32),
            agc2: agc2((v >> 12) as usize),
        };
        let commands = DspCommands {
            reset: (v >> 38) % 40 == 0,
            flush: (v >> 44) % 30 == 0,
            gain_reset: (v >> 50) % 20 == 0,
            gain_reset_db: ((v >> 56) % 40) as f32 - 10.0,
        };
        let frame = &mut frame[..len];
        for s in frame.iter_mut() {
            *s = if step % 500 < 250 { 32767.0 * rng.symmetric() } else { 3.0e5 * rng.symmetric() };
        }
        let m = ((v >> 40) % (8 * 480 + 501)) as usize;
        let output = match (v >> 42) % 3 {
            0 => DenoiserOutput::NotInUse,
            1 => DenoiserOutput::Missed,
            _ => DenoiserOutput::Ran { samples: &denoised[..m], restart: (v >> 46) % 7 == 0 },
        };
        let context = format!("step {step} rate {rate} len {len}");
        match (v >> 52) % 10 {
            0 => {
                chain.post_denoise(frame, output, &settings, &mut out);
            }
            1 => {
                chain.pre_denoise(frame, &settings, &commands, &mut out);
            }
            2 => chain.process(frame, &settings, &commands, &mut out),
            3 | 4 => {
                chain.pre_denoise(frame, &settings, &commands, &mut out);
                let aligned = chain.post_denoise_align(frame, output, &settings, &mut out);
                for s in frame.iter_mut() {
                    *s *= 30.0;
                }
                let gain_db = [30.0, f32::NAN, f32::INFINITY, -1e30, 1e30][(v % 5) as usize];
                chain.post_gain(frame, aligned.pre_gain_rms, gain_db, &settings, &mut out);
            }
            5 => {
                let gain_db = [12.0, f32::NAN][(v % 2) as usize];
                chain.post_gain(frame, 0.1, gain_db, &settings, &mut out);
            }
            _ => {
                chain.pre_denoise(frame, &settings, &commands, &mut out);
                chain.post_denoise(frame, output, &settings, &mut out);
            }
        }
        if (v >> 52) % 10 != 1 {
            check(frame, &out, &context);
        }
    }
}
