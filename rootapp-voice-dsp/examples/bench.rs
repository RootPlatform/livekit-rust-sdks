#![allow(clippy::disallowed_macros, clippy::arithmetic_side_effects, clippy::indexing_slicing)]

//! 10 000 frames at 48 kHz through every stage, the denoiser output going through the aligner.
//! cargo run --release -p rootapp-voice-dsp --example bench

use rootapp_voice_dsp::{DenoiserOutput, DspChain, DspCommands, DspOutputs, DspSettings};
use std::hint::black_box;
use std::time::{Duration, Instant};

const FRAME: usize = 480;
const FRAMES: usize = 10_000;
const WARMUP: usize = 500;

fn main() {
    let total = FRAMES + WARMUP;
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    };
    let input: Vec<f32> = (0..total * FRAME)
        .map(|i| {
            let syllable = (i / FRAME) % 30 < 20;
            let tone = if syllable { 3000.0 * (i as f32 * 0.0288).sin() } else { 0.0 };
            tone + 10.0 * noise()
        })
        .collect();
    let denoised: Vec<f32> = input.iter().map(|v| (v * 0.5).round()).collect();

    let mut chain = DspChain::new(48_000);
    let mut out = DspOutputs::default();
    let settings = DspSettings { denoiser_wanted: true, ..DspSettings::default() };
    let commands = DspCommands::default();
    let mut frame = [0.0f32; FRAME];
    let mut elapsed = Duration::ZERO;
    let mut worst = Duration::ZERO;
    let mut sent = 0.0f64;

    for f in 0..total {
        frame.copy_from_slice(&input[f * FRAME..(f + 1) * FRAME]);
        let samples = match f {
            0 => &denoised[..0],
            5 => &denoised[(f - 2) * FRAME..f * FRAME],
            _ => &denoised[(f - 1) * FRAME..f * FRAME],
        };
        let start = Instant::now();
        chain.pre_denoise(black_box(&mut frame), &settings, &commands, &mut out);
        chain.post_denoise(
            &mut frame,
            DenoiserOutput::Ran { samples: black_box(samples), restart: f == 0 },
            &settings,
            &mut out,
        );
        let took = start.elapsed();
        black_box(&frame);
        if f >= WARMUP {
            elapsed += took;
            worst = worst.max(took);
            sent += out.sent_sumsq;
        }
    }

    let per_frame = elapsed.as_nanos() as f64 / FRAMES as f64;
    println!(
        "{FRAMES} frames at 48 kHz: {per_frame:.0} ns/frame mean, {} ns worst, {:.3}% of real time \
         (speaking {}, gain {:.1} dB, aligner drained {} underruns {}, sent {:.3e})",
        worst.as_nanos(),
        per_frame / 10_000_000.0 * 100.0,
        out.speaking,
        out.voice_gain_db,
        out.aligner.drained_samples,
        out.aligner.underruns,
        sent
    );
}
