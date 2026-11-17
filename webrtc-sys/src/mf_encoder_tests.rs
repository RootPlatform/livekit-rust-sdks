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

//! The MF H.264 encoder (`src/mf/h264_encoder_impl.cpp`) on this machine's GPU. The scenarios
//! live in `src/mf/mf_encoder_harness.cpp`; each test runs one and reports what it found.

use std::ffi::{c_char, CStr, CString};

extern "C" {
    fn lk_mf_encoder_test(scenario: *const c_char, message: *mut c_char, message_len: usize)
        -> i32;
}

/// For scenarios that compare a texture's device with the encoder's: the shared device must not be
/// retired while they run.
fn run_on_shared_device(scenario: &str) {
    let _registry =
        crate::mf_device::DEVICE_REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    run(scenario);
}

fn run(scenario: &str) {
    let name = CString::new(scenario).unwrap();
    let mut message = vec![0 as c_char; 2048];
    let result = unsafe { lk_mf_encoder_test(name.as_ptr(), message.as_mut_ptr(), message.len()) };
    let message = unsafe { CStr::from_ptr(message.as_ptr()) }.to_string_lossy().into_owned();
    match result {
        0 => {
            if !message.is_empty() {
                eprintln!("{scenario}: {message}");
            }
        }
        2 => eprintln!("{scenario}: skipped (no MF hardware H.264 encoder)"),
        _ => panic!("{scenario}: {message}"),
    }
}

/// A frame the sink refuses after the encoder delivered it (a paused simulcast layer) is not an
/// encode error, and the encoder keeps delivering once the sink accepts again.
#[test]
fn a_refused_send_is_not_an_encode_error() {
    run("refused_send");
}

/// A GPU too busy to run the encoder for 1.5 s (a game hitching) drops frames instead of moving
/// the share to OpenH264 for the rest of the session, and encoding carries on afterwards.
#[test]
fn a_briefly_stalled_hardware_encoder_stays_on_hardware() {
    run("slow_hardware");
}

/// A D3D11 texture on the encoder's device is converted and scaled to NV12 on the GPU; the
/// decoded frames carry the colours libyuv gives the memory path, at full size and at a scaled
/// layer.
#[test]
fn a_d3d11_texture_encodes_on_the_gpu() {
    run_on_shared_device("gpu_input");
}

/// The D3D11 video processor turns RGBA bars into NV12 with the colours libyuv gives, at full
/// size and scaled to half.
#[test]
fn the_video_processor_converts_like_libyuv() {
    run_on_shared_device("gpu_convert");
}

/// A D3D11 frame reads back to the same I420 libyuv makes from the pixels, and its lazy
/// crop-and-scale stays a texture until read.
#[test]
fn a_d3d11_frame_reads_back_like_libyuv() {
    run_on_shared_device("gpu_readback");
}

/// A texture on another device than the encoder's is read back to memory and still encodes in
/// hardware.
#[test]
fn a_d3d11_texture_on_another_device_falls_back_to_memory() {
    run_on_shared_device("gpu_other_device");
}

/// An encoder that makes no progress for longer than the limit still falls back to software.
#[test]
fn a_stuck_hardware_encoder_still_falls_back() {
    run("stuck_hardware");
}
