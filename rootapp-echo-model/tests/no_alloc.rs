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

use rootapp_echo_model::{Estimator, Model, Tuning, AEC3_BINS, AEC3_BLOCK};

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
fn estimate_block_and_reset_never_allocate_or_free() {
    let model = Arc::new(Model::embedded().unwrap());
    let blocks = 4000;
    let (render, linear) = scene::scene(blocks * AEC3_BLOCK);
    let tunings = [Tuning::default(), Tuning { render_gate_blocks: 75, ..Tuning::default() }];
    for tuning in tunings {
        let mut estimator = Estimator::new(model.clone(), tuning);
        let mut fraction = [0.0f32; AEC3_BINS];
        let before = allocations();
        let mut inferences = 0;
        for b in 0..blocks {
            let range = b * AEC3_BLOCK..(b + 1) * AEC3_BLOCK;
            if estimator.estimate_block(&render[range.clone()], &linear[range], &mut fraction) {
                inferences += 1;
            }
            if b % 1000 == 999 {
                estimator.reset();
            }
            if b == 2000 {
                estimator.estimate_block(
                    &[f32::NAN; AEC3_BLOCK],
                    &[1e30; AEC3_BLOCK],
                    &mut fraction,
                );
            }
        }
        assert_eq!(allocations(), before, "{tuning:?}");
        assert!(inferences >= blocks / 2 - 4);
    }
}
