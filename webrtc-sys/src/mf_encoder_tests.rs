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
