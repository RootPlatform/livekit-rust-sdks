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

//! MFDecodeWatchdog (`src/mf/mf_decode_watchdog.h`). The scenarios live in
//! `src/mf/mf_decode_watchdog_harness.cpp`; each test runs one and reports
//! what it found.

use std::ffi::{c_char, CStr, CString};

extern "C" {
    fn lk_mf_decode_watchdog_test(
        scenario: *const c_char,
        message: *mut c_char,
        message_len: usize,
    ) -> i32;
}

fn run(scenario: &str) {
    let name = CString::new(scenario).unwrap();
    let mut message = vec![0 as c_char; 2048];
    let result =
        unsafe { lk_mf_decode_watchdog_test(name.as_ptr(), message.as_mut_ptr(), message.len()) };
    let message = unsafe { CStr::from_ptr(message.as_ptr()) }.to_string_lossy().into_owned();
    match result {
        0 => {
            if !message.is_empty() {
                eprintln!("{scenario}: {message}");
            }
        }
        2 => eprintln!("{scenario}: skipped (no MF hardware decoder or software H264 encoder)"),
        _ => panic!("{scenario}: {message}"),
    }
}

#[test]
fn watchdog_passes_frames_through() {
    run("passes_through");
}

#[test]
fn watchdog_abandons_a_hung_delta_frame_and_asks_for_a_keyframe() {
    run("hang_on_delta");
}

#[test]
fn watchdog_hands_a_hung_keyframe_to_software() {
    run("hang_on_keyframe");
}

#[test]
fn watchdog_gives_up_on_a_silent_decoder_for_its_stream() {
    run("silent");
}

#[test]
fn watchdog_abandons_a_hung_configure() {
    run("configure_hang");
}

#[test]
fn watchdog_behind_webrtc_software_fallback() {
    run("behind_fallback_wrapper");
}

#[test]
fn watchdog_over_the_real_mf_decoder() {
    run("real_decoder");
    // Only the real decoder reports to the process-wide counts the FFI exports.
    let (mut hangs, mut silences) = (u32::MAX, u32::MAX);
    unsafe { crate::mf_device::lk_mf_decode_watchdog_counts(&mut hangs, &mut silences) };
    assert_eq!((hangs, silences), (0, 0));
}
