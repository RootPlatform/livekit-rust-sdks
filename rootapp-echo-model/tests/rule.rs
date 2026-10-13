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

//! The spec's rule (estimator-spec.md section 8.3) on synthetic hook inputs
//! with a fixed model fraction: the floor, the three guards, and the model
//! counting only while the far end is active.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rootapp_echo_model::{Hook, Rule, TodayMasks, AEC3_BINS};

/// One block's inputs; spectra flat per bin.
#[derive(Clone)]
struct Block {
    y: [f32; 64],
    e: [f32; 64],
    s2: [f32; 65],
    y2: [f32; 65],
    e2: [f32; 65],
    dominant_nearend: bool,
    active: bool,
}

impl Block {
    fn new(s2: f32, y2: f32, e2: f32) -> Block {
        Block {
            y: [0.0; 64],
            e: [0.0; 64],
            s2: [s2; 65],
            y2: [y2; 65],
            e2: [e2; 65],
            dominant_nearend: false,
            active: false,
        }
    }

    fn active(mut self, active: bool) -> Block {
        self.active = active;
        self
    }

    fn nearend(mut self, nearend: bool) -> Block {
        self.dominant_nearend = nearend;
        self
    }
}

fn apply(rule: &mut Rule, b: &Block, fraction: f32, guard_blocks: u32) -> [f32; 65] {
    let render = [0.0f32; 64];
    let hook = Hook {
        render: &render,
        y: &b.y,
        e: &b.e,
        s2: &b.s2,
        y2: &b.y2,
        e2: &b.e2,
        dominant_nearend: b.dominant_nearend,
        far_end_active: b.active,
        onset_guard_blocks: guard_blocks,
    };
    let mut r2 = [f32::NAN; 65];
    rule.apply(&hook, &[fraction; AEC3_BINS], &mut r2);
    r2
}

/// Σ Y2 above the level gate (64² · 30²) for every block below.
const Y2: f32 = 1e6;

#[test]
fn far_end_inactive_and_no_linear_echo_estimate_is_no_residual() {
    let mut rule = Rule::new();
    for _ in 0..50 {
        let r2 = apply(&mut rule, &Block::new(0.0, Y2, 0.3 * Y2), 1.0, 15);
        assert!(r2.iter().all(|&v| v == 0.0), "{r2:?}");
    }
}

#[test]
fn the_model_counts_only_while_the_far_end_is_active() {
    let mut rule = Rule::new();
    let block = Block::new(0.0, Y2, 0.3 * Y2);
    apply(&mut rule, &block.clone().active(true), 0.5, 0);
    let on = apply(&mut rule, &block.clone().active(true), 0.5, 0);
    assert!(on.iter().all(|&v| v == 0.3 * Y2 * 0.5), "{on:?}");
    let off = apply(&mut rule, &block, 0.5, 0);
    assert!(off.iter().all(|&v| v == 0.0), "{off:?}");
}

#[test]
fn saturated_echo_suppresses_that_block_only() {
    let mut rule = Rule::new();
    let mut saturated = Block::new(0.0, Y2, 0.3 * Y2);
    saturated.y[5] = 32000.0;
    saturated.e[5] = 32000.0 - 20001.0;
    let r2 = apply(&mut rule, &saturated, 0.0, 0);
    assert!(r2.iter().zip(&saturated.y2).all(|(r, y)| r >= y), "{r2:?}");
    let r2 = apply(&mut rule, &Block::new(0.0, Y2, 0.3 * Y2), 0.0, 0);
    assert!(r2.iter().all(|&v| v == 0.0), "the next block: {r2:?}");
    let mut short = saturated.clone();
    short.e[5] = 32000.0 - 20000.0;
    let r2 = apply(&mut rule, &short, 0.0, 0);
    assert!(r2.iter().all(|&v| v == 0.0), "y - e at 20000 is not saturated echo: {r2:?}");
    let mut quiet = saturated;
    quiet.y[5] = 31999.0;
    quiet.e[5] = 31999.0 - 20001.0;
    let r2 = apply(&mut rule, &quiet, 0.0, 0);
    assert!(r2.iter().all(|&v| v == 0.0), "a capture under 32000 is not saturated: {r2:?}");
}

#[test]
fn a_diverged_filter_suppresses_that_block_and_the_next_four() {
    for nearend in [false, true] {
        let mut rule = Rule::new();
        let calm = Block::new(0.0, Y2, 0.1 * Y2).active(true).nearend(nearend);
        for _ in 0..10 {
            assert!(apply(&mut rule, &calm, 0.0, 0).iter().all(|&v| v == 0.0));
        }
        let diverged = Block::new(0.0, Y2, 2.0 * Y2).active(true).nearend(nearend);
        let r2 = apply(&mut rule, &diverged, 0.0, 0);
        if nearend {
            assert!(r2.iter().all(|&v| v == 0.0), "no guard in dominant nearend: {r2:?}");
            continue;
        }
        assert!(r2.iter().all(|&v| v == 2.0 * Y2), "{r2:?}");
        for b in 1..=4 {
            let r2 = apply(&mut rule, &calm, 0.0, 0);
            assert!(r2.iter().all(|&v| v == 0.1 * Y2), "block {b} after: {r2:?}");
        }
        let r2 = apply(&mut rule, &calm, 0.0, 0);
        assert!(r2.iter().all(|&v| v == 0.0), "the fifth block after: {r2:?}");
    }
    let mut rule = Rule::new();
    let inactive = Block::new(0.0, Y2, 2.0 * Y2);
    assert!(apply(&mut rule, &inactive, 0.0, 0).iter().all(|&v| v == 0.0), "far end inactive");
    let quiet = Block::new(0.0, 100.0, 200.0).active(true);
    assert!(apply(&mut rule, &quiet, 0.0, 0).iter().all(|&v| v == 0.0), "under the level gate");
}

#[test]
fn the_onset_guard_suppresses_the_guard_blocks_after_the_far_end_turns_active() {
    let guard = 15;
    let idle = Block::new(0.0, Y2, 0.1 * Y2);
    let talking = idle.clone().active(true);
    let mut rule = Rule::new();
    for _ in 0..3 {
        assert!(apply(&mut rule, &idle, 0.0, guard).iter().all(|&v| v == 0.0));
    }
    for b in 0..guard {
        let r2 = apply(&mut rule, &talking, 0.0, guard);
        assert!(r2.iter().all(|&v| v == 0.1 * Y2), "guard block {b}: {r2:?}");
    }
    for _ in 0..20 {
        assert!(apply(&mut rule, &talking, 0.0, guard).iter().all(|&v| v == 0.0));
    }
    assert!(apply(&mut rule, &idle, 0.0, guard).iter().all(|&v| v == 0.0));
    for b in 0..guard {
        let nearend = b < 5;
        let r2 = apply(&mut rule, &talking.clone().nearend(nearend), 0.0, guard);
        let want = if nearend { 0.0 } else { 0.1 * Y2 };
        assert!(r2.iter().all(|&v| v == want), "block {b} of the second onset: {r2:?}");
    }
    assert!(apply(&mut rule, &talking, 0.0, guard).iter().all(|&v| v == 0.0));
}

#[test]
fn the_floor_scales_aec3s_estimate_by_todays_enr_suppress() {
    let x = 1000.0f32;
    for (nearend, bin3, bin10) in [(false, 0.4f32, 0.1f32), (true, 1.1, 0.3)] {
        let mut rule = Rule::new();
        rule.set_today_masks(&TodayMasks::default());
        let mut block = Block::new(0.0, 0.0, 0.0).nearend(nearend);
        block.s2[3] = x;
        block.s2[10] = x;
        let r2 = apply(&mut rule, &block, 0.0, 0);
        let close = |got: f32, want: f32| (got - want).abs() <= 1e-6 * want;
        assert!(close(r2[3], x / bin3), "nearend {nearend}: bin 3 {} vs {}", r2[3], x / bin3);
        assert!(close(r2[10], x / bin10), "nearend {nearend}: bin 10 {} vs {}", r2[10], x / bin10);
        assert_eq!(r2[0], 0.0);
    }
    let mut rule = Rule::new();
    rule.set_today_masks(&TodayMasks { normal_lf: 0.8, normal_hf: 0.2, ..TodayMasks::default() });
    let mut block = Block::new(0.0, 0.0, 0.0);
    block.s2[3] = x;
    block.s2[10] = x;
    let r2 = apply(&mut rule, &block, 0.0, 0);
    assert!((r2[3] - x / 0.8).abs() <= 1e-3 && (r2[10] - x / 0.2).abs() <= 1e-2, "{r2:?}");
}

#[test]
fn every_capture_channel_gets_its_own_estimate_from_the_shared_state() {
    let mut rule = Rule::new();
    let render = [0.0f32; 64];
    let y = [0.0f32; 64];
    let mut s2 = [0.0f32; 130];
    let mut y2 = [Y2; 130];
    let mut e2 = [0.1 * Y2; 130];
    s2[65 + 3] = 7.0;
    y2[65..].fill(2.0 * Y2);
    e2[65..].fill(0.2 * Y2);
    let hook = Hook {
        render: &render,
        y: &y,
        e: &y,
        s2: &s2,
        y2: &y2,
        e2: &e2,
        dominant_nearend: false,
        far_end_active: true,
        onset_guard_blocks: 0,
    };
    let mut r2 = [0.0f32; 130];
    rule.apply(&hook, &[0.5; AEC3_BINS], &mut r2);
    assert!(r2[..65].iter().all(|&v| v == 0.1 * Y2 * 0.5), "{:?}", &r2[..65]);
    assert_eq!(r2[65 + 3], (7.0f32 / 0.4).max(0.2 * Y2 * 0.5));
    assert!(r2[65..].iter().enumerate().all(|(k, &v)| k == 3 || v == 0.2 * Y2 * 0.5));
}
