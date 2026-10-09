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

use std::sync::Mutex;

pub use cxx::SharedPtr;

use crate::impl_thread_safety;

/// `init_result` / `start_result` before an attempt.
pub const ADM_RESULT_NOT_ATTEMPTED: i32 = 1;
/// `init_result` when the selected device is not present.
pub const ADM_RESULT_DEVICE_NOT_FOUND: i32 = -2;

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(u8)]
    pub enum AudioDeviceSelectionKind {
        Default = 0,
        Guid = 1,
        Name = 2,
        Index = 3,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AudioDeviceSelection {
        pub kind: AudioDeviceSelectionKind,
        pub id: String,
        pub index: u16,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AudioDeviceOutcome {
        pub requested: bool,
        pub found: bool,
        pub attempted: bool,
        pub init_result: i32,
        pub start_result: i32,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AudioDeviceRequestOutcome {
        pub recording: AudioDeviceOutcome,
        pub playout: AudioDeviceOutcome,
    }

    #[derive(Debug, Clone, Default)]
    pub struct RootAudioNativeStats {
        pub capture_callbacks: u64,
        pub render_callbacks: u64,
        pub capture_max_gap_us: u64,
        pub render_max_gap_us: u64,
        pub last_capture_age_us: u64,
        pub last_render_age_us: u64,
        pub recording: bool,
        pub playing: bool,
        pub recording_init_result: i32,
        pub recording_start_result: i32,
        pub playout_init_result: i32,
        pub playout_start_result: i32,
        pub capture_rate: u32,
        pub capture_channels: u32,
        pub capture_raw_sumsq: f64,
        pub capture_raw_samples: u64,
        pub playout_delay_ms: u32,
        pub recording_delay_ms: u32,
        pub mix_calls: u64,
        pub mix_max_gap_us: u64,
        pub mix_sources: u32,
        pub mix_ssrcs: Vec<u32>,
        pub mix_out_sumsq: f64,
        pub mix_out_samples: u64,
    }

    unsafe extern "C++" {
        include!("livekit/audio_device_controller.h");
        include!("livekit/peer_connection_factory.h");

        type AudioDeviceController;
        type PeerConnectionFactory = crate::peer_connection_factory::ffi::PeerConnectionFactory;

        fn audio_device(self: &PeerConnectionFactory) -> SharedPtr<AudioDeviceController>;

        fn playout_devices(self: &AudioDeviceController) -> i16;
        fn recording_devices(self: &AudioDeviceController) -> i16;
        fn playout_device_name(self: &AudioDeviceController, index: u16) -> String;
        fn recording_device_name(self: &AudioDeviceController, index: u16) -> String;
        fn playout_device_guid(self: &AudioDeviceController, index: u16) -> String;
        fn recording_device_guid(self: &AudioDeviceController, index: u16) -> String;

        fn set_playout_device(self: &AudioDeviceController, index: u16) -> bool;
        fn set_recording_device(self: &AudioDeviceController, index: u16) -> bool;
        fn set_playout_device_by_guid(self: &AudioDeviceController, guid: String) -> bool;
        fn set_recording_device_by_guid(self: &AudioDeviceController, guid: String) -> bool;

        fn select_devices(
            self: &AudioDeviceController,
            recording: &AudioDeviceSelection,
            set_recording: bool,
            playout: &AudioDeviceSelection,
            set_playout: bool,
            callback: Box<AudioDeviceRequestCallback>,
        );
        fn restart_devices(
            self: &AudioDeviceController,
            recording: bool,
            playout: bool,
            callback: Box<AudioDeviceRequestCallback>,
        );
        fn set_playout_hold(self: &AudioDeviceController, hold: bool);
        fn root_audio_stats(self: &AudioDeviceController) -> RootAudioNativeStats;

        fn stop_recording(self: &AudioDeviceController) -> bool;
        fn init_recording(self: &AudioDeviceController) -> bool;
        fn start_recording(self: &AudioDeviceController) -> bool;
        fn recording_is_initialized(self: &AudioDeviceController) -> bool;

        fn stop_playout(self: &AudioDeviceController) -> bool;
        fn init_playout(self: &AudioDeviceController) -> bool;
        fn start_playout(self: &AudioDeviceController) -> bool;
        fn playout_is_initialized(self: &AudioDeviceController) -> bool;

        fn builtin_aec_is_available(self: &AudioDeviceController) -> bool;
        fn builtin_agc_is_available(self: &AudioDeviceController) -> bool;
        fn builtin_ns_is_available(self: &AudioDeviceController) -> bool;
        fn enable_builtin_aec(self: &AudioDeviceController, enable: bool) -> bool;
        fn enable_builtin_agc(self: &AudioDeviceController, enable: bool) -> bool;
        fn enable_builtin_ns(self: &AudioDeviceController, enable: bool) -> bool;

        fn set_adm_recording_enabled(self: &AudioDeviceController, enabled: bool);
        fn adm_recording_enabled(self: &AudioDeviceController) -> bool;

        fn set_adm_playout_enabled(self: &AudioDeviceController, enabled: bool);
        fn adm_playout_enabled(self: &AudioDeviceController) -> bool;

        fn acquire_platform_adm(self: &AudioDeviceController) -> bool;
        fn release_platform_adm(self: &AudioDeviceController);
        fn platform_adm_ref_count(self: &AudioDeviceController) -> i32;
        fn is_platform_adm_active(self: &AudioDeviceController) -> bool;

        fn resolve_audio_device_index_for_test(
            selection: &AudioDeviceSelection,
            names: &[String],
            guids: &[String],
        ) -> i32;
    }

    extern "Rust" {
        type AudioDeviceRequestCallback;

        fn on_complete(self: &AudioDeviceRequestCallback, outcome: AudioDeviceRequestOutcome);
    }
}

impl_thread_safety!(ffi::AudioDeviceController, Send + Sync);

/// Completion of a device request, called once on the native device thread.
pub struct AudioDeviceRequestCallback {
    on_complete: Mutex<Option<Box<dyn FnOnce(ffi::AudioDeviceRequestOutcome) + Send>>>,
}

impl AudioDeviceRequestCallback {
    pub fn new(on_complete: impl FnOnce(ffi::AudioDeviceRequestOutcome) + Send + 'static) -> Self {
        Self { on_complete: Mutex::new(Some(Box::new(on_complete))) }
    }

    fn on_complete(&self, outcome: ffi::AudioDeviceRequestOutcome) {
        let callback = self.on_complete.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(callback) = callback {
            callback(outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ffi::{
        resolve_audio_device_index_for_test, AudioDeviceSelection, AudioDeviceSelectionKind,
    };
    use crate::peer_connection_factory::ffi::create_peer_connection_factory;
    use std::sync::Mutex;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    const NOT_FOUND: i32 = -2;
    const DEFAULT: i32 = -1;

    fn resolve(kind: AudioDeviceSelectionKind, id: &str, index: u16) -> i32 {
        let names: Vec<String> =
            ["Speakers", "Headset", "Headset"].iter().map(|s| s.to_string()).collect();
        let guids: Vec<String> = ["{a}", "{b}", "{c}"].iter().map(|s| s.to_string()).collect();
        resolve_audio_device_index_for_test(
            &AudioDeviceSelection { kind, id: id.to_string(), index },
            &names,
            &guids,
        )
    }

    #[test]
    fn device_selection_default_resolves_to_the_os_default() {
        assert_eq!(resolve(AudioDeviceSelectionKind::Default, "", 0), DEFAULT);
        assert_eq!(resolve(AudioDeviceSelectionKind::Default, "{b}", 2), DEFAULT);
    }

    #[test]
    fn device_selection_by_guid_finds_the_current_index() {
        assert_eq!(resolve(AudioDeviceSelectionKind::Guid, "{a}", 0), 0);
        assert_eq!(resolve(AudioDeviceSelectionKind::Guid, "{c}", 0), 2);
    }

    #[test]
    fn device_selection_by_name_takes_the_first_match() {
        assert_eq!(resolve(AudioDeviceSelectionKind::Name, "Headset", 0), 1);
        assert_eq!(resolve(AudioDeviceSelectionKind::Name, "Speakers", 0), 0);
    }

    #[test]
    fn device_selection_missing_device_is_not_found_never_index_zero() {
        assert_eq!(resolve(AudioDeviceSelectionKind::Guid, "{gone}", 0), NOT_FOUND);
        assert_eq!(resolve(AudioDeviceSelectionKind::Name, "USB Mic", 0), NOT_FOUND);
        assert_eq!(resolve(AudioDeviceSelectionKind::Guid, "", 0), NOT_FOUND);
        assert_eq!(resolve(AudioDeviceSelectionKind::Name, "", 0), NOT_FOUND);
        assert_eq!(resolve(AudioDeviceSelectionKind::Guid, "Headset", 0), NOT_FOUND);
    }

    #[test]
    fn device_selection_by_index_checks_the_range() {
        assert_eq!(resolve(AudioDeviceSelectionKind::Index, "", 1), 1);
        assert_eq!(resolve(AudioDeviceSelectionKind::Index, "", 3), NOT_FOUND);
    }

    #[test]
    fn device_selection_with_no_devices_is_not_found() {
        let none: Vec<String> = Vec::new();
        let selection = AudioDeviceSelection {
            kind: AudioDeviceSelectionKind::Guid,
            id: "{a}".to_string(),
            index: 0,
        };
        assert_eq!(resolve_audio_device_index_for_test(&selection, &none, &none), NOT_FOUND);
    }

    #[test]
    fn test_audio_device_controller_bridge() {
        if std::env::var_os("LK_HW_AUDIO_TESTS").is_none() {
            eprintln!("skipped: creates the platform ADM; set LK_HW_AUDIO_TESTS=1 to run");
            return;
        }
        let _guard = TEST_MUTEX.lock().expect("test mutex poisoned");
        let factory = create_peer_connection_factory();
        let audio = factory.audio_device();

        let recording_count = audio.recording_devices();
        let playout_count = audio.playout_devices();
        assert!(recording_count >= 0);
        assert!(playout_count >= 0);

        if recording_count > 0 {
            let _ = audio.recording_device_name(0);
            let guid = audio.recording_device_guid(0);
            let _ = audio.set_recording_device_by_guid(guid);
        }

        if playout_count > 0 {
            let _ = audio.playout_device_name(0);
            let guid = audio.playout_device_guid(0);
            let _ = audio.set_playout_device_by_guid(guid);
        }

        let initial_recording = audio.adm_recording_enabled();
        audio.set_adm_recording_enabled(!initial_recording);
        assert_eq!(audio.adm_recording_enabled(), !initial_recording);
        audio.set_adm_recording_enabled(initial_recording);
        assert_eq!(audio.adm_recording_enabled(), initial_recording);

        let initial_playout = audio.adm_playout_enabled();
        audio.set_adm_playout_enabled(!initial_playout);
        assert_eq!(audio.adm_playout_enabled(), !initial_playout);
        audio.set_adm_playout_enabled(initial_playout);
        assert_eq!(audio.adm_playout_enabled(), initial_playout);

        assert!(audio.platform_adm_ref_count() >= 0);
        let _ = audio.is_platform_adm_active();
    }
}
