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

#![allow(clippy::unwrap_used, unsafe_code)]

#[path = "support/scene.rs"]
mod scene;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use rootapp_echo_model::{Estimator, Hook, Model, TodayMasks, AEC3_BINS, AEC3_BLOCK};

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        System.realloc(ptr, layout, size)
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

#[test]
fn estimate_and_reset_never_allocate_or_free() {
    let model = Arc::new(Model::embedded().unwrap());
    let blocks = 4000;
    let (render, linear) = scene::scene(blocks * AEC3_BLOCK);
    let mut estimator = Estimator::new(model);
    let mut r2 = [0.0f32; AEC3_BINS * 2];
    let mut y = [0.0f32; AEC3_BLOCK];
    let mut spectra = [[0.0f32; AEC3_BINS * 2]; 3];
    let before = allocations();
    estimator.set_today_masks(&TodayMasks::default());
    let mut inferences = 0;
    for b in 0..blocks {
        let range = b * AEC3_BLOCK..(b + 1) * AEC3_BLOCK;
        for (i, v) in y.iter_mut().enumerate() {
            *v = linear[range.start + i] + 0.3 * render[range.start + i];
        }
        for (s, spectrum) in spectra.iter_mut().enumerate() {
            for (k, v) in spectrum.iter_mut().enumerate() {
                *v = (1 + s) as f32 * 1e5 * (1.0 + ((b + k) % 7) as f32);
            }
        }
        let [s2, y2, e2] = &spectra;
        let hook = Hook {
            render: &render[range.clone()],
            y: &y,
            e: &linear[range],
            s2,
            y2,
            e2,
            dominant_nearend: b % 30 < 5,
            far_end_active: b % 200 > 20,
            onset_guard_blocks: 15,
        };
        if estimator.estimate(&hook, &mut r2) {
            inferences += 1;
        }
        if b % 1000 == 999 {
            estimator.reset();
        }
        if b == 2000 {
            let nan = [f32::NAN; AEC3_BLOCK];
            let huge = [1e30f32; AEC3_BLOCK];
            let hostile = Hook { render: &nan, y: &huge, e: &huge, ..hook };
            estimator.estimate(&hostile, &mut r2);
        }
    }
    assert_eq!(allocations(), before);
    assert!(inferences >= blocks / 2 - 4);
}
