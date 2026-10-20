// Copyright 2025 LiveKit, Inc.
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

use crate::impl_thread_safety;

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    #[derive(Debug)]
    #[repr(i32)]
    pub enum MediaType {
        Audio,
        Video,
        Data,
        Unsupported,
    }

    #[derive(Debug)]
    #[repr(i32)]
    pub enum Priority {
        VeryLow,
        Low,
        Medium,
        High,
    }

    #[derive(Debug)]
    #[repr(i32)]
    pub enum RtpTransceiverDirection {
        SendRecv,
        SendOnly,
        RecvOnly,
        Inactive,
        Stopped,
    }

    #[derive(Debug)]
    #[repr(i32)]
    pub enum LoggingSeverity {
        Verbose,
        Info,
        Warning,
        Error,
        None,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(i32)]
    pub enum VideoEncoderBackend {
        Auto,
        Software,
        Hardware,
        Nvenc,
        Vaapi,
        VideoToolbox,
        PreEncoded,
    }

    unsafe extern "C++" {
        include!("livekit/webrtc.h");

        type LogSink;

        fn create_random_uuid() -> String;
        fn video_encoder_backend_list() -> Vec<VideoEncoderBackend>;
        fn new_log_sink(
            fnc: fn(String, LoggingSeverity),
            min_severity: LoggingSeverity,
        ) -> UniquePtr<LogSink>;
        // None stops forwarding; libwebrtc skips formatting lines below the minimum.
        fn set_min_severity(self: Pin<&mut LogSink>, severity: LoggingSeverity);
    }

    #[cfg(feature = "harness")]
    unsafe extern "C++" {
        include!("livekit/webrtc.h");

        /// libwebrtc's global minimum severity (LogMessage::GetMinLogSeverity).
        fn log_min_severity_for_test() -> LoggingSeverity;
        /// RTC_LOG_V(severity) << message; whether libwebrtc formatted the line.
        fn log_for_test(severity: LoggingSeverity, message: &str) -> bool;
    }
}

impl_thread_safety!(ffi::LogSink, Send + Sync);

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::ffi::{log_for_test, log_min_severity_for_test, new_log_sink, LoggingSeverity};

    const MARK: &str = "webrtc-sys log floor test";

    static RECEIVED: Mutex<Vec<(String, LoggingSeverity)>> = Mutex::new(Vec::new());

    fn on_log(message: String, severity: LoggingSeverity) {
        if message.contains(MARK) {
            RECEIVED.lock().unwrap().push((message, severity));
        }
    }

    fn log(severity: LoggingSeverity, n: u32) -> bool {
        log_for_test(severity, &format!("{MARK} {n}"))
    }

    fn received() -> Vec<(u32, LoggingSeverity)> {
        RECEIVED
            .lock()
            .unwrap()
            .drain(..)
            .map(|(message, severity)| {
                let n = message.trim_end().rsplit(' ').next().unwrap().parse().unwrap();
                (n, severity)
            })
            .collect()
    }

    // libwebrtc's logging state is process-wide, and no other test here adds a sink.
    #[test]
    fn libwebrtc_formats_only_what_the_sink_forwards() {
        assert_eq!(log_min_severity_for_test(), LoggingSeverity::None);
        assert!(!log(LoggingSeverity::Error, 0));

        let mut sink = new_log_sink(on_log, LoggingSeverity::Warning);
        assert_eq!(log_min_severity_for_test(), LoggingSeverity::Warning);
        assert!(!log(LoggingSeverity::Verbose, 1));
        assert!(!log(LoggingSeverity::Info, 2));
        assert!(log(LoggingSeverity::Warning, 3));
        assert!(log(LoggingSeverity::Error, 4));
        assert_eq!(received(), [(3, LoggingSeverity::Warning), (4, LoggingSeverity::Error)]);

        sink.pin_mut().set_min_severity(LoggingSeverity::Info);
        assert_eq!(log_min_severity_for_test(), LoggingSeverity::Info);
        assert!(!log(LoggingSeverity::Verbose, 5));
        assert!(log(LoggingSeverity::Info, 6));
        assert_eq!(received(), [(6, LoggingSeverity::Info)]);

        sink.pin_mut().set_min_severity(LoggingSeverity::Error);
        assert_eq!(log_min_severity_for_test(), LoggingSeverity::Error);
        assert!(!log(LoggingSeverity::Warning, 7));
        assert!(log(LoggingSeverity::Error, 8));
        assert_eq!(received(), [(8, LoggingSeverity::Error)]);

        sink.pin_mut().set_min_severity(LoggingSeverity::None);
        assert_eq!(log_min_severity_for_test(), LoggingSeverity::None);
        assert!(!log(LoggingSeverity::Error, 9));

        sink.pin_mut().set_min_severity(LoggingSeverity::Warning);
        drop(sink);
        assert_eq!(log_min_severity_for_test(), LoggingSeverity::None);
        assert!(!log(LoggingSeverity::Error, 10));
        assert!(received().is_empty());
    }
}
