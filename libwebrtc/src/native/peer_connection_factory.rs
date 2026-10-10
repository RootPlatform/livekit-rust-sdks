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

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use cxx::{SharedPtr, UniquePtr};
use lazy_static::lazy_static;
use parking_lot::Mutex;
use tokio::sync::oneshot;
use webrtc_sys::{
    audio_device_controller as sys_adc, peer_connection_factory as sys_pcf, rtc_error as sys_err,
    webrtc as sys_rtc,
};

pub use webrtc_sys::audio_device_controller::{
    ffi::RootAudioCaptureFormat as NativeCaptureFormat,
    ffi::RootAudioControlUpdate as NativeAudioControl,
    ffi::RootAudioNativeStats as NativeAudioStats,
    ffi::RootDenoiserCompletionItem as NativeDenoiserCompletion,
    ffi::RootDenoiserReturnedItem as NativeReturnedPipeline,
    ffi::RootDenoiserStateInfo as NativeDenoiserState,
    ffi::RootReturnPipelinesResult as NativeReturnPipelines,
    ffi::RootSpeakingInfo as NativeSpeaking, ADM_RESULT_DEVICE_NOT_FOUND, ADM_RESULT_NOT_ATTEMPTED,
};

/// What became of an effect played on the native effect bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectPlay {
    Queued,
    /// No clip was registered under that id.
    UnknownClip,
    /// The queue of 32 effects waiting for the render thread was full.
    Dropped,
    /// No platform audio reference exists, so nothing plays it.
    Idle,
}

/// What a mic test monitor request did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorResult {
    Applied,
    /// This build has no capture processor (mobile).
    NotSupported,
    /// A send stream records from the platform microphone (a call publishes).
    Sending,
    /// No platform audio reference exists.
    NotAcquired,
    /// The platform audio device module could not be created or initialized.
    NoDevice,
}

use crate::{
    audio_source::{native::NativeAudioSource, AudioSourceOptions},
    audio_track::RtcAudioTrack,
    imp::{audio_track as imp_at, peer_connection as imp_pc, video_track as imp_vt},
    peer_connection::PeerConnection,
    peer_connection_factory::RtcConfiguration,
    rtp_parameters::RtpCapabilities,
    video_source::native::NativeVideoSource,
    video_track::RtcVideoTrack,
    MediaType, RtcError, RtcErrorType,
};

/// Which device a direction of the Platform ADM opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioDeviceSelection {
    /// The OS default (the console role on Windows, index 0 elsewhere).
    SystemDefault,
    /// The ADM GUID (Windows: MMDevice id; macOS: AudioObjectID as a string).
    Guid(String),
    /// The ADM device name (Linux, where the ADM leaves GUIDs empty).
    Name(String),
    /// The macOS CoreAudio device UID, translated to the device's current
    /// AudioObjectID at every open; no device elsewhere.
    Uid(String),
}

/// What a device request did for one direction. Results are ADM return codes:
/// 0 is success, [`ADM_RESULT_NOT_ATTEMPTED`] means no attempt was made (the
/// direction is not running, or was already running on that device) and
/// [`ADM_RESULT_DEVICE_NOT_FOUND`] means the selected device is not present. A
/// failed stop leaves the direction closed until the native retry a second
/// later, so its init and start are not attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDeviceOutcome {
    pub found: bool,
    pub attempted: bool,
    pub stop_result: i32,
    pub init_result: i32,
    pub start_result: i32,
}

/// Per direction; `None` when the request did not cover it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioDeviceRequestOutcome {
    pub recording: Option<AudioDeviceOutcome>,
    pub playout: Option<AudioDeviceOutcome>,
}

impl From<sys_adc::ffi::AudioDeviceRequestOutcome> for AudioDeviceRequestOutcome {
    fn from(value: sys_adc::ffi::AudioDeviceRequestOutcome) -> Self {
        let direction = |outcome: sys_adc::ffi::AudioDeviceOutcome| {
            outcome.requested.then_some(AudioDeviceOutcome {
                found: outcome.found,
                attempted: outcome.attempted,
                stop_result: outcome.stop_result,
                init_result: outcome.init_result,
                start_result: outcome.start_result,
            })
        };
        Self { recording: direction(value.recording), playout: direction(value.playout) }
    }
}

fn to_sys_selection(
    selection: Option<&AudioDeviceSelection>,
) -> sys_adc::ffi::AudioDeviceSelection {
    let (kind, id) = match selection {
        Some(AudioDeviceSelection::Guid(guid)) => {
            (sys_adc::ffi::AudioDeviceSelectionKind::Guid, guid.clone())
        }
        Some(AudioDeviceSelection::Name(name)) => {
            (sys_adc::ffi::AudioDeviceSelectionKind::Name, name.clone())
        }
        Some(AudioDeviceSelection::Uid(uid)) => {
            (sys_adc::ffi::AudioDeviceSelectionKind::Uid, uid.clone())
        }
        Some(AudioDeviceSelection::SystemDefault) | None => {
            (sys_adc::ffi::AudioDeviceSelectionKind::Default, String::new())
        }
    };
    sys_adc::ffi::AudioDeviceSelection { kind, id, index: 0 }
}

/// Completes on the native device thread once the request is applied, or with
/// `None` if the device thread went away first.
pub struct AudioDeviceRequest {
    rx: oneshot::Receiver<AudioDeviceRequestOutcome>,
}

impl AudioDeviceRequest {
    fn channel() -> (Self, Box<sys_adc::AudioDeviceRequestCallback>) {
        let (tx, rx) = oneshot::channel();
        let callback = sys_adc::AudioDeviceRequestCallback::new(move |outcome| {
            let _ = tx.send(outcome.into());
        });
        (Self { rx }, Box::new(callback))
    }
}

impl Future for AudioDeviceRequest {
    type Output = Option<AudioDeviceRequestOutcome>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.rx).poll(cx).map(Result::ok)
    }
}

struct LogSinkState {
    sink: Option<UniquePtr<sys_rtc::ffi::LogSink>>,
    level: log::LevelFilter,
}

lazy_static! {
    static ref LOG_SINK: Mutex<LogSinkState> =
        Mutex::new(LogSinkState { sink: None, level: log::LevelFilter::Trace });
}

// libwebrtc's warnings and errors keep their level; LS_INFO is logged at debug and
// LS_VERBOSE at trace.
fn native_log_severity(level: log::LevelFilter) -> sys_rtc::ffi::LoggingSeverity {
    match level {
        log::LevelFilter::Off => sys_rtc::ffi::LoggingSeverity::None,
        log::LevelFilter::Error => sys_rtc::ffi::LoggingSeverity::Error,
        log::LevelFilter::Warn | log::LevelFilter::Info => sys_rtc::ffi::LoggingSeverity::Warning,
        log::LevelFilter::Debug => sys_rtc::ffi::LoggingSeverity::Info,
        log::LevelFilter::Trace => sys_rtc::ffi::LoggingSeverity::Verbose,
    }
}

/// The most verbose level libwebrtc's own log lines are forwarded to the `log` facade at
/// (target `libwebrtc`). libwebrtc does not format lines below it. Defaults to trace.
pub fn set_log_level(level: log::LevelFilter) {
    let mut state = LOG_SINK.lock();
    state.level = level;
    if let Some(sink) = state.sink.as_mut() {
        sink.pin_mut().set_min_severity(native_log_severity(level));
    }
}

fn ensure_log_sink() {
    let mut state = LOG_SINK.lock();
    if state.sink.is_none() {
        let severity = native_log_severity(state.level);
        state.sink = Some(sys_rtc::ffi::new_log_sink(
            |msg, severity| {
                let msg = msg.strip_suffix("\r\n").or(msg.strip_suffix('\n')).unwrap_or(&msg);
                let level = match severity {
                    sys_rtc::ffi::LoggingSeverity::Error => log::Level::Error,
                    sys_rtc::ffi::LoggingSeverity::Warning => log::Level::Warn,
                    sys_rtc::ffi::LoggingSeverity::Info => log::Level::Debug,
                    _ => log::Level::Trace,
                };
                log::log!(target: "libwebrtc", level, "{}", msg);
            },
            severity,
        ));
    }
}

#[derive(Clone)]
pub struct PeerConnectionFactory {
    pub(crate) sys_handle: SharedPtr<sys_pcf::ffi::PeerConnectionFactory>,
}

impl Default for PeerConnectionFactory {
    fn default() -> Self {
        ensure_log_sink();
        let sys_handle = sys_pcf::ffi::create_peer_connection_factory();
        Self { sys_handle }
    }
}

impl PeerConnectionFactory {
    /// Creates a [`PeerConnectionFactory`] with the WebRTC-ForcePlayoutDelay field trial enabled.
    pub fn with_zero_playout_delay() -> Self {
        ensure_log_sink();
        let sys_handle = sys_pcf::ffi::create_peer_connection_factory_with_zero_playout_delay();
        Self { sys_handle }
    }

    /// Creates a [`PeerConnectionFactory`] with the given runtime options.
    /// `zero_playout_delay` enables the WebRTC-ForcePlayoutDelay field trial;
    /// `enable_warp` enables WARP (SPED via the WebRTC-IceHandshakeDtls field
    /// trial; SNAP is carried on the RtcConfiguration, not a field trial). The
    /// two are independent and may be combined.
    pub fn with_options(zero_playout_delay: bool, enable_warp: bool) -> Self {
        ensure_log_sink();
        let sys_handle = sys_pcf::ffi::create_peer_connection_factory_with_options(
            zero_playout_delay,
            enable_warp,
        );
        Self { sys_handle }
    }

    #[cfg(test)]
    pub(crate) fn zero_playout_delay_enabled(&self) -> bool {
        self.sys_handle.zero_playout_delay_enabled()
    }

    pub fn create_peer_connection(
        &self,
        config: RtcConfiguration,
    ) -> Result<PeerConnection, RtcError> {
        let observer = Arc::new(imp_pc::PeerObserver::default());
        let res = self.sys_handle.create_peer_connection(
            config.into(),
            Box::new(sys_pcf::PeerConnectionObserverWrapper::new(observer.clone())),
        );

        match res {
            Ok(sys_handle) => Ok(PeerConnection {
                handle: imp_pc::PeerConnection::configure(sys_handle, observer),
            }),
            Err(e) => Err(unsafe { sys_err::ffi::RtcError::from(e.what()).into() }),
        }
    }

    pub fn create_video_track(&self, label: &str, source: NativeVideoSource) -> RtcVideoTrack {
        RtcVideoTrack {
            handle: imp_vt::RtcVideoTrack::new(
                self.sys_handle.create_video_track(label.to_string(), source.handle.sys_handle()),
            ),
        }
    }

    pub fn create_audio_track(&self, label: &str, source: NativeAudioSource) -> RtcAudioTrack {
        RtcAudioTrack {
            handle: imp_at::RtcAudioTrack {
                sys_handle: self
                    .sys_handle
                    .create_audio_track(label.to_string(), source.handle.sys_handle()),
            },
        }
    }

    /// Create an audio track that uses the Platform ADM for capture.
    ///
    /// The track captures from the selected recording device and owns the
    /// shared APM's settings: WebRTC's software echo canceller and noise
    /// suppressor as `options` ask, the high-pass filter, and never AGC
    /// (`options.auto_gain_control` is ignored).
    pub fn create_device_audio_track(
        &self,
        label: &str,
        options: &AudioSourceOptions,
    ) -> Result<RtcAudioTrack, RtcError> {
        let sys_handle = self.sys_handle.create_device_audio_track(
            label.to_string(),
            options.echo_cancellation,
            options.noise_suppression,
        );
        if sys_handle.is_null() {
            return Err(RtcError {
                error_type: RtcErrorType::Internal,
                message: "failed to create the device audio track".to_owned(),
            });
        }
        Ok(RtcAudioTrack { handle: imp_at::RtcAudioTrack { sys_handle } })
    }

    pub fn get_rtp_sender_capabilities(&self, media_type: MediaType) -> RtpCapabilities {
        self.sys_handle.rtp_sender_capabilities(media_type.into()).into()
    }

    pub fn get_rtp_receiver_capabilities(&self, media_type: MediaType) -> RtpCapabilities {
        self.sys_handle.rtp_receiver_capabilities(media_type.into()).into()
    }

    // ===== Device Management Methods =====

    /// Get the number of playout (output) devices
    pub fn playout_devices(&self) -> i16 {
        self.sys_handle.audio_device().playout_devices()
    }

    /// Get the number of recording (input) devices
    pub fn recording_devices(&self) -> i16 {
        self.sys_handle.audio_device().recording_devices()
    }

    /// Get the name of a playout device by index
    pub fn playout_device_name(&self, index: u16) -> String {
        self.sys_handle.audio_device().playout_device_name(index)
    }

    /// Get the name of a recording device by index
    pub fn recording_device_name(&self, index: u16) -> String {
        self.sys_handle.audio_device().recording_device_name(index)
    }

    /// Get the GUID of a playout device by index
    /// The GUID is a platform-specific unique identifier that is stable across device hot-plug events.
    pub fn playout_device_guid(&self, index: u16) -> String {
        self.sys_handle.audio_device().playout_device_guid(index)
    }

    /// Get the GUID of a recording device by index
    /// The GUID is a platform-specific unique identifier that is stable across device hot-plug events.
    pub fn recording_device_guid(&self, index: u16) -> String {
        self.sys_handle.audio_device().recording_device_guid(index)
    }

    /// Set the playout device by index
    pub fn set_playout_device(&self, index: u16) -> bool {
        self.sys_handle.audio_device().set_playout_device(index)
    }

    /// Set the recording device by index
    pub fn set_recording_device(&self, index: u16) -> bool {
        self.sys_handle.audio_device().set_recording_device(index)
    }

    /// Set the playout device by GUID
    /// This is preferred over index as GUIDs are stable across device hot-plug events.
    pub fn set_playout_device_by_guid(&self, guid: &str) -> bool {
        self.sys_handle.audio_device().set_playout_device_by_guid(guid.to_string())
    }

    /// Set the recording device by GUID
    /// This is preferred over index as GUIDs are stable across device hot-plug events.
    pub fn set_recording_device_by_guid(&self, guid: &str) -> bool {
        self.sys_handle.audio_device().set_recording_device_by_guid(guid.to_string())
    }

    /// Stop recording (clears initialized state, allowing device switch)
    pub fn stop_recording(&self) -> bool {
        self.sys_handle.audio_device().stop_recording()
    }

    /// Initialize recording
    pub fn init_recording(&self) -> bool {
        self.sys_handle.audio_device().init_recording()
    }

    /// Start recording
    pub fn start_recording(&self) -> bool {
        self.sys_handle.audio_device().start_recording()
    }

    /// Check if recording is initialized
    pub fn recording_is_initialized(&self) -> bool {
        self.sys_handle.audio_device().recording_is_initialized()
    }

    /// Stop playout (clears initialized state, allowing device switch)
    pub fn stop_playout(&self) -> bool {
        self.sys_handle.audio_device().stop_playout()
    }

    /// Initialize playout
    pub fn init_playout(&self) -> bool {
        self.sys_handle.audio_device().init_playout()
    }

    /// Start playout
    pub fn start_playout(&self) -> bool {
        self.sys_handle.audio_device().start_playout()
    }

    /// Check if playout is initialized
    pub fn playout_is_initialized(&self) -> bool {
        self.sys_handle.audio_device().playout_is_initialized()
    }

    // ===== Built-in Audio Processing Methods =====
    // These control hardware AEC/AGC/NS on platforms that support it (iOS, some Android)

    /// Check if built-in (hardware) AEC is available on this device.
    ///
    /// Returns true on iOS (VPIO) and some Android devices.
    /// Returns false on desktop platforms (macOS, Windows, Linux).
    pub fn builtin_aec_is_available(&self) -> bool {
        self.sys_handle.audio_device().builtin_aec_is_available()
    }

    /// Check if built-in (hardware) AGC is available on this device.
    ///
    /// Returns true on iOS (VPIO) and some Android devices.
    /// Returns false on desktop platforms (macOS, Windows, Linux).
    pub fn builtin_agc_is_available(&self) -> bool {
        self.sys_handle.audio_device().builtin_agc_is_available()
    }

    /// Check if built-in (hardware) NS is available on this device.
    ///
    /// Returns true on iOS (VPIO) and some Android devices.
    /// Returns false on desktop platforms (macOS, Windows, Linux).
    pub fn builtin_ns_is_available(&self) -> bool {
        self.sys_handle.audio_device().builtin_ns_is_available()
    }

    /// Enable or disable built-in (hardware) AEC.
    ///
    /// When disabled on platforms that support it, WebRTC's software AEC
    /// will be used instead.
    pub fn enable_builtin_aec(&self, enable: bool) -> bool {
        self.sys_handle.audio_device().enable_builtin_aec(enable)
    }

    /// Enable or disable built-in (hardware) AGC.
    ///
    /// When disabled on platforms that support it, WebRTC's software AGC
    /// will be used instead.
    pub fn enable_builtin_agc(&self, enable: bool) -> bool {
        self.sys_handle.audio_device().enable_builtin_agc(enable)
    }

    /// Enable or disable built-in (hardware) NS.
    ///
    /// When disabled on platforms that support it, WebRTC's software NS
    /// will be used instead.
    pub fn enable_builtin_ns(&self, enable: bool) -> bool {
        self.sys_handle.audio_device().enable_builtin_ns(enable)
    }

    /// Control whether ADM recording (microphone) is enabled.
    ///
    /// When disabled, WebRTC's calls to InitRecording/StartRecording will be no-ops.
    /// Use this when only using NativeAudioSource (no microphone capture needed).
    /// This prevents the microphone from interfering with the audio pipeline.
    pub fn set_adm_recording_enabled(&self, enabled: bool) {
        self.sys_handle.audio_device().set_adm_recording_enabled(enabled)
    }

    /// Check if ADM recording (microphone) is enabled.
    pub fn adm_recording_enabled(&self) -> bool {
        self.sys_handle.audio_device().adm_recording_enabled()
    }

    /// Control whether ADM playout (speakers) is enabled.
    ///
    /// When disabled (default), playout uses synthetic mode - remote audio is
    /// delivered via FFI callbacks to the application (e.g., Unity AudioSource).
    /// When enabled, remote audio plays through the platform speakers with AEC.
    pub fn set_adm_playout_enabled(&self, enabled: bool) {
        self.sys_handle.audio_device().set_adm_playout_enabled(enabled)
    }

    /// Check if ADM playout (speakers) is enabled.
    pub fn adm_playout_enabled(&self) -> bool {
        self.sys_handle.audio_device().adm_playout_enabled()
    }

    // ===== Platform ADM Lifecycle Management =====

    /// Acquires a reference to the Platform ADM.
    ///
    /// On first call, creates and initializes the Platform ADM. On subsequent
    /// calls, just increments the reference count.
    ///
    /// Returns true if Platform ADM is ready for use, false if initialization failed.
    pub fn acquire_platform_adm(&self) -> bool {
        self.sys_handle.audio_device().acquire_platform_adm()
    }

    /// Releases a reference to the Platform ADM.
    ///
    /// When the reference count reaches zero, the Platform ADM is terminated
    /// and the proxy returns to synthetic mode.
    pub fn release_platform_adm(&self) {
        self.sys_handle.audio_device().release_platform_adm()
    }

    /// Returns the current reference count for the Platform ADM.
    pub fn platform_adm_ref_count(&self) -> i32 {
        self.sys_handle.audio_device().platform_adm_ref_count()
    }

    /// Returns true if Platform ADM is currently active (ref_count > 0).
    pub fn is_platform_adm_active(&self) -> bool {
        self.sys_handle.audio_device().is_platform_adm_active()
    }

    // ===== RootApp native audio =====

    /// Stores the device selection per direction (`None` keeps it) and moves a
    /// running direction to the new device on the native device thread.
    pub fn select_audio_devices(
        &self,
        recording: Option<AudioDeviceSelection>,
        playout: Option<AudioDeviceSelection>,
    ) -> AudioDeviceRequest {
        let (request, callback) = AudioDeviceRequest::channel();
        self.sys_handle.audio_device().select_devices(
            &to_sys_selection(recording.as_ref()),
            recording.is_some(),
            &to_sys_selection(playout.as_ref()),
            playout.is_some(),
            callback,
        );
        request
    }

    /// Stops, re-selects, re-inits and restarts the running directions.
    pub fn restart_audio_devices(&self, recording: bool, playout: bool) -> AudioDeviceRequest {
        let (request, callback) = AudioDeviceRequest::channel();
        self.sys_handle.audio_device().restart_devices(recording, playout, callback);
        request
    }

    /// Keeps platform playout running with no receive stream.
    pub fn set_audio_playout_hold(&self, hold: bool) {
        self.sys_handle.audio_device().set_playout_hold(hold)
    }

    /// Device and mixer counters; reads atomics only. Max gaps reset on read.
    pub fn root_audio_stats(&self) -> NativeAudioStats {
        self.sys_handle.audio_device().root_audio_stats()
    }

    /// Live control of the capture gate, input and output volume, deafen and
    /// the APM's AEC/NS policy. Never blocks on an audio thread. False when a
    /// set field has nothing to act on in this build (the others are applied).
    pub fn set_root_audio_control(&self, control: &NativeAudioControl) -> bool {
        self.sys_handle.audio_device().set_root_audio_control(control)
    }

    /// Tile volume (linear 0..2) and pause of the remote stream with this SSRC;
    /// false when the mixer's table is full.
    pub fn set_root_audio_source_gain(&self, ssrc: u32, tile_volume: f32, paused: bool) -> bool {
        self.sys_handle.audio_device().set_root_audio_source_gain(ssrc, tile_volume, paused)
    }

    pub fn clear_root_audio_source_gain(&self, ssrc: u32) {
        self.sys_handle.audio_device().clear_root_audio_source_gain(ssrc)
    }

    /// Copies a 48 kHz mono clip; false when it is empty or too long.
    pub fn register_root_audio_effect(&self, id: u32, samples: &[i16]) -> bool {
        self.sys_handle.audio_device().register_root_audio_effect(id, samples)
    }

    pub fn play_root_audio_effect(&self, id: u32, gain: f32) -> EffectPlay {
        match self.sys_handle.audio_device().play_root_audio_effect(id, gain) {
            0 => EffectPlay::Queued,
            1 => EffectPlay::UnknownClip,
            2 => EffectPlay::Dropped,
            _ => EffectPlay::Idle,
        }
    }

    pub fn stop_root_audio_effects(&self) {
        self.sys_handle.audio_device().stop_root_audio_effects()
    }

    pub fn root_audio_capture_format(&self) -> NativeCaptureFormat {
        self.sys_handle.audio_device().root_audio_capture_format()
    }

    /// Moves whenever the playout mixer gets a new source.
    pub fn root_audio_sources_added(&self) -> u64 {
        self.sys_handle.audio_device().root_audio_sources_added()
    }

    /// Controls back to their defaults, with the capture gate open or closed.
    pub fn reset_root_audio(&self, close_gate: bool) {
        self.sys_handle.audio_device().reset_root_audio(close_gate)
    }

    /// Starts or stops the mic test monitor; returns at once, the devices
    /// follow on the native device thread.
    pub fn set_root_audio_monitor(&self, enabled: bool) -> MonitorResult {
        match self.sys_handle.audio_device().set_root_audio_monitor(enabled) {
            0 => MonitorResult::Applied,
            2 => MonitorResult::Sending,
            3 => MonitorResult::NotAcquired,
            4 => MonitorResult::NoDevice,
            _ => MonitorResult::NotSupported,
        }
    }

    /// The capture processor has an Effects SDK slot (desktop, 64-bit).
    pub fn root_denoiser_available(&self) -> bool {
        self.sys_handle.audio_device().root_denoiser_available()
    }

    /// Queues an Effects SDK pipeline install (`pipeline` non-zero) or removal
    /// for `owner`; it completes through `take_root_denoiser_completions`.
    /// False without a slot.
    pub fn set_root_denoiser(
        &self,
        owner: u64,
        async_id: u64,
        pipeline: u64,
        sample_rate: u32,
    ) -> bool {
        self.sys_handle.audio_device().set_root_denoiser(owner, async_id, pipeline, sample_rate)
    }

    /// Takes back every pipeline native holds; waits up to 200 ms for the
    /// audio thread.
    pub fn return_root_denoiser_pipelines(&self) -> NativeReturnPipelines {
        self.sys_handle.audio_device().return_root_denoiser_pipelines()
    }

    pub fn take_root_denoiser_completions(&self) -> Vec<NativeDenoiserCompletion> {
        self.sys_handle.audio_device().take_root_denoiser_completions()
    }

    pub fn take_root_denoiser_returned(&self) -> Vec<NativeReturnedPipeline> {
        self.sys_handle.audio_device().take_root_denoiser_returned()
    }

    pub fn root_denoiser_state(&self) -> NativeDenoiserState {
        self.sys_handle.audio_device().root_denoiser_state()
    }

    /// The published speaking flag; atomics only.
    pub fn root_audio_speaking(&self) -> NativeSpeaking {
        self.sys_handle.audio_device().root_audio_speaking()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    #[tokio::test]
    async fn test_peer_connection_factory_and_audio_device_controller_bridge() {
        let _guard = TEST_MUTEX.lock().expect("test mutex poisoned");
        let _ = env_logger::builder().is_test(true).try_init();

        let factory = PeerConnectionFactory::default();
        let source = NativeVideoSource::default();
        let _track = factory.create_video_track("test", source);
        let recording_count = factory.recording_devices();
        let playout_count = factory.playout_devices();

        assert!(recording_count >= 0);
        assert!(playout_count >= 0);

        let initial_recording = factory.adm_recording_enabled();
        factory.set_adm_recording_enabled(!initial_recording);
        assert_eq!(factory.adm_recording_enabled(), !initial_recording);
        factory.set_adm_recording_enabled(initial_recording);
    }
}
