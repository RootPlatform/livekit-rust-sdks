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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/scene.rs"]
mod scene;

use std::sync::Arc;

use rootapp_echo_model::{
    dtln::{process_offline, Dtln},
    weights, Estimator, Hook, Kernel, Model, ModelFraction, AEC3_BINS, AEC3_BLOCK, SCALE,
};
use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn model() -> Arc<Model> {
    Arc::new(Model::embedded().unwrap())
}

/// The model's fraction at `scale` after every block of the scene.
fn fractions(model: Arc<Model>, scale: f32, blocks: usize) -> Vec<[f32; AEC3_BINS]> {
    let (render, linear) = scene::scene(blocks * AEC3_BLOCK);
    let mut m = ModelFraction::new(model, scale);
    let mut out = Vec::new();
    for b in 0..blocks {
        let range = b * AEC3_BLOCK..(b + 1) * AEC3_BLOCK;
        m.block(&render[range.clone()], &linear[range]);
        out.push(*m.fraction());
    }
    out
}

#[test]
fn weights_are_the_published_files() {
    assert_eq!(hex(weights::STAGE1), weights::STAGE1_SHA256);
    assert_eq!(hex(weights::STAGE2), weights::STAGE2_SHA256);
    assert_eq!((weights::STAGE1.len(), weights::STAGE2.len()), (1_996_600, 5_277_012));
    assert!(weights::LICENSE.starts_with("MIT License"));
    assert!(weights::LICENSE.contains("Nils L. Westhausen"));
    assert!(weights::SOURCE.contains("breizhn/DTLN-aec"));
}

#[test]
fn the_embedded_model_is_dtln_aec_128() {
    let model = Model::embedded().unwrap();
    assert_eq!(model.params(), 494_982 + 1_314_817);
    assert!(model.macs() > 1_000_000);
    assert_eq!(model.kernel(), Kernel::detect());
    assert!(rootapp_echo_model::prepare());
    assert!(rootapp_echo_model::prepared());
    assert!(rootapp_echo_model::shared().is_some());
}

#[test]
fn malformed_models_are_errors_not_panics() {
    for bytes in [&b""[..], &[0u8; 3][..], &[0xffu8; 64][..], &weights::STAGE1[..1000]] {
        assert!(Model::from_tflite(bytes, weights::STAGE2).is_err());
        assert!(Model::from_tflite(weights::STAGE1, bytes).is_err());
    }
    assert!(Model::from_tflite(weights::STAGE2, weights::STAGE1).is_err());
}

/// The fractions the research runtime (scratchpad `echo/dtln`, verified
/// against the authors' outputs) gives on the same scene, x2.
#[test]
fn model_fraction_matches_the_research_runtime() {
    let golden: Vec<f32> = include_bytes!("data/golden-x2.f32")
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let hops = golden.len() / AEC3_BINS;
    let ours = fractions(model(), 2.0, hops * 2);
    let (mut max, mut sum, mut n) = (0.0f32, 0.0f64, 0usize);
    let mut below_one = 0;
    for hop in 0..hops {
        let mine = &ours[hop * 2 + 1];
        let theirs = &golden[hop * AEC3_BINS..(hop + 1) * AEC3_BINS];
        for (a, b) in mine.iter().zip(theirs) {
            let d = (a - b).abs();
            max = max.max(d);
            sum += f64::from(d);
            n += 1;
            if *b < 0.99 {
                below_one += 1;
            }
        }
    }
    assert!(below_one > n / 4, "the scene exercises the model ({below_one} of {n} below 1)");
    assert!(max < 2e-3, "max difference {max}");
    assert!(sum / (n as f64) < 1e-5, "mean difference {}", sum / n as f64);
}

#[test]
fn every_kernel_gives_the_same_fraction() {
    let reference = fractions(
        Arc::new(Model::embedded().unwrap().with_kernel(Kernel::Scalar).unwrap()),
        SCALE,
        400,
    );
    for kernel in [Kernel::Sse2, Kernel::Avx2Fma, Kernel::Neon] {
        let Ok(model) = Model::embedded().unwrap().with_kernel(kernel) else {
            assert!(!kernel.available());
            continue;
        };
        let other = fractions(Arc::new(model), SCALE, 400);
        let max = reference
            .iter()
            .zip(&other)
            .flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x - y).abs()))
            .fold(0.0f32, f32::max);
        assert!(max < 1e-3, "{kernel:?}: max difference {max}");
    }
}

#[test]
fn kernels_compute_dot_products() {
    let mut noise = scene::Lcg::new(3);
    for n in [0usize, 1, 3, 4, 7, 8, 15, 16, 31, 32, 33, 64, 127, 257, 512] {
        let a: Vec<f32> = (0..n).map(|_| noise.next()).collect();
        let b: Vec<f32> = (0..n).map(|_| noise.next()).collect();
        let exact: f64 = a.iter().zip(&b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum();
        for kernel in [Kernel::Scalar, Kernel::Sse2, Kernel::Avx2Fma, Kernel::Neon] {
            if !kernel.available() {
                continue;
            }
            let got = f64::from(kernel.dot(&a, &b));
            assert!(
                (got - exact).abs() < 1e-4 * (1.0 + exact.abs()),
                "{kernel:?} n={n}: {got} vs {exact}"
            );
            assert_eq!(kernel.dot(&a, &b[..n / 2]), kernel.dot(&a[..n / 2], &b[..n / 2]));
        }
    }
    if cfg!(target_arch = "x86_64") {
        assert!(Kernel::Sse2.available());
        assert!(!Kernel::Neon.available());
    }
    if cfg!(target_arch = "aarch64") {
        assert_eq!(Kernel::detect(), Kernel::Neon);
    }
}

/// AEC3-like hook inputs for block `b` of the scene: the render, the mic as
/// echo plus near voice, the linear output, and their spectra (Y2 from the
/// mic, E2 from the linear output, S2 from their difference).
fn hook_spectra(
    render: &[f32],
    linear: &[f32],
    b: usize,
) -> ([f32; 64], [f32; 64], [f32; 64], [[f32; 65]; 3]) {
    let r: [f32; 64] = render[b * 64..(b + 1) * 64].try_into().unwrap();
    let e: [f32; 64] = linear[b * 64..(b + 1) * 64].try_into().unwrap();
    let mut y = [0.0f32; 64];
    for i in 0..64 {
        y[i] = e[i] + 0.2 * r[i];
    }
    let power = |x: &[f32; 64]| -> [f32; 65] {
        let mut out = [0.0f32; 65];
        for (k, o) in out.iter_mut().enumerate() {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, v) in x.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * n) as f64 / 128.0;
                re += f64::from(*v) * a.cos();
                im += f64::from(*v) * a.sin();
            }
            *o = (re * re + im * im) as f32;
        }
        out
    };
    let mut s = [0.0f32; 64];
    for i in 0..64 {
        s[i] = y[i] - e[i];
    }
    (r, y, e, [power(&s), power(&y), power(&e)])
}

fn run_estimator(e: &mut Estimator, blocks: usize) -> (Vec<[f32; 65]>, usize) {
    let (render, linear) = scene::scene(blocks * AEC3_BLOCK);
    let mut all = Vec::new();
    let mut inferences = 0;
    for b in 0..blocks {
        let (r, y, lin, [s2, y2, e2]) = hook_spectra(&render, &linear, b);
        let hook = Hook {
            render: &r,
            y: &y,
            e: &lin,
            s2: &s2,
            y2: &y2,
            e2: &e2,
            dominant_nearend: b % 50 > 40,
            far_end_active: b % 100 > 10,
            onset_guard_blocks: 15,
        };
        let mut r2 = [0.0f32; 65];
        if e.estimate(&hook, &mut r2) {
            inferences += 1;
        }
        all.push(r2);
    }
    (all, inferences)
}

#[test]
fn the_estimate_starts_at_one_and_reset_repeats_it_exactly() {
    let m = ModelFraction::new(model(), SCALE);
    assert!(m.fraction().iter().all(|&f| f == 1.0));
    let mut estimator = Estimator::new(model());
    let (a, inferences) = run_estimator(&mut estimator, 200);
    estimator.reset();
    let (b, _) = run_estimator(&mut estimator, 200);
    assert_eq!(inferences, 100);
    assert_eq!(a, b);
    assert!(a.iter().flatten().all(|v| v.is_finite() && *v >= 0.0));
}

#[test]
fn hostile_input_never_panics_and_keeps_the_fraction_in_range() {
    let mut m = ModelFraction::new(model(), SCALE);
    let blocks: [[f32; AEC3_BLOCK]; 6] = [
        [f32::NAN; AEC3_BLOCK],
        [f32::INFINITY; AEC3_BLOCK],
        [f32::NEG_INFINITY; AEC3_BLOCK],
        [1e30; AEC3_BLOCK],
        [-1e30; AEC3_BLOCK],
        [32767.0; AEC3_BLOCK],
    ];
    for _ in 0..20 {
        for x in &blocks {
            for y in &blocks {
                m.block(x, y);
                assert!(m.fraction().iter().all(|v| (0.0..=1.0).contains(v)), "{:?}", m.fraction());
            }
        }
    }
    m.block(&[1.0; 3], &[]);
    let mut estimator = Estimator::new(model());
    let spectrum = [1e30f32; 65];
    let short = [1.0f32; 10];
    let mut r2 = [0.0f32; 65 * 2];
    for x in &blocks {
        let hook = Hook {
            render: x,
            y: x,
            e: &short,
            s2: &spectrum,
            y2: &short,
            e2: &spectrum,
            dominant_nearend: false,
            far_end_active: true,
            onset_guard_blocks: 15,
        };
        estimator.estimate(&hook, &mut r2);
        estimator.estimate(&hook, &mut r2[..3]);
        estimator.estimate(&hook, &mut []);
    }
}

/// The authors' published outputs of DTLN-aec-512 (the only size they
/// published outputs for), through this executor: within 1 LSB except where
/// their output clipped. Needs `ROOTAPP_DTLN_REFERENCE_DIR`, a checkout of
/// the DTLN-aec repository (`pretrained_models/`, `audio_samples/`):
/// `cargo test -- --ignored` with it set.
#[test]
#[ignore = "needs ROOTAPP_DTLN_REFERENCE_DIR (a DTLN-aec checkout)"]
fn executor_matches_the_authors_outputs_within_one_lsb() {
    let dir = std::env::var_os("ROOTAPP_DTLN_REFERENCE_DIR")
        .expect("ROOTAPP_DTLN_REFERENCE_DIR is not set: this test cannot run without it");
    let dir = std::path::PathBuf::from(dir);
    let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
    let model = Arc::new(
        Model::from_tflite(
            &read("pretrained_models/dtln_aec_512_1.tflite"),
            &read("pretrained_models/dtln_aec_512_2.tflite"),
        )
        .unwrap(),
    );
    let mut dtln = Dtln::new(model);
    for clip in [
        "9mkQhVtzTEy2hDk-6u2Sww_farend_singletalk",
        "DLhjtuwiEkS-68TsUVvW5g_nearend_singletalk",
        "DMTgmZwtgUilp4omPK7-OQ_doubletalk",
    ] {
        // soundfile reads PCM16 as x / 32768 and writes float as round(x * 32768), clipped.
        let wav = |kind: &str| wav16(&read(&format!("audio_samples/{clip}_{kind}.wav")));
        let (mic, lpb, reference) = (wav("mic"), wav("lpb"), wav("processed"));
        let unit = |v: &[i16]| v.iter().map(|&s| f32::from(s) / 32768.0).collect::<Vec<_>>();
        dtln.reset();
        let out = process_offline(&mut dtln, &unit(&mic), &unit(&lpb));
        let n = out.len().min(reference.len());
        let (mut worst, mut clipped) = (0i32, 0);
        for i in 0..n {
            if reference[i] == i16::MAX || reference[i] == i16::MIN {
                clipped += 1;
                continue;
            }
            let ours = (out[i].clamp(-1.0, 1.0) * 32768.0).round() as i32;
            worst = worst.max((ours - i32::from(reference[i])).abs());
        }
        assert!(worst <= 1, "{clip}: {worst} LSB");
        eprintln!("{clip}: {n} samples, worst {worst} LSB, {clipped} clipped in the reference");
    }
}

/// The samples of a 16-bit PCM mono WAV file.
fn wav16(bytes: &[u8]) -> Vec<i16> {
    let mut p = 12;
    while p + 8 <= bytes.len() {
        let id = &bytes[p..p + 4];
        let size =
            u32::from_le_bytes([bytes[p + 4], bytes[p + 5], bytes[p + 6], bytes[p + 7]]) as usize;
        if id == b"data" {
            let end = (p + 8 + size).min(bytes.len());
            return bytes[p + 8..end]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
        }
        p += 8 + size + (size & 1);
    }
    panic!("no data chunk");
}
