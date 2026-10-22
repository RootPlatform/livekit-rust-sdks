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

//! FFI bindings for PlatformAudio device management.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use livekit::{PlatformAudio, PlayoutDeviceId, RecordingDeviceId};

use super::{rootapp_audio::RootAudioSession, FfiHandle, FfiServer};
use crate::{proto, FfiResult};

/// FFI wrapper for PlatformAudio handle.
pub struct FfiPlatformAudio {
    pub audio: PlatformAudio,
    /// RootApp native audio state of this handle (track gains, events).
    pub rootapp: Arc<RootAudioSession>,
}

impl FfiHandle for FfiPlatformAudio {}

impl Drop for FfiPlatformAudio {
    // Runs in livekit_ffi_drop_handle, before the host can create the next one.
    fn drop(&mut self) {
        self.rootapp.end();
    }
}

/// A clone of the handle's `PlatformAudio`, so no handle-map lock is held while
/// it enumerates devices or waits on the ADM.
pub fn platform_audio_from_handle(
    server: &'static FfiServer,
    handle: u64,
) -> FfiResult<PlatformAudio> {
    Ok(server.retrieve_handle::<FfiPlatformAudio>(handle)?.audio.clone())
}

/// A NewPlatformAudio at or above this is logged as a warning.
const SLOW_CREATE: Duration = Duration::from_secs(2);

// Runs on the caller's thread. Nothing here holds a lock a room connect
// needs: the runtime is built without the Platform ADM, which is created on
// the device thread by the first PlatformAudio (see livekit's
// PlatformAudio::new, which logs its own steps).
pub fn on_new_platform_audio(
    server: &'static FfiServer,
    _req: proto::NewPlatformAudioRequest,
) -> FfiResult<proto::NewPlatformAudioResponse> {
    log::info!("[PLATFORM_AUDIO_FFI] on_new_platform_audio() called");
    let started = Instant::now();

    match PlatformAudio::new() {
        Ok(audio) => {
            let created = started.elapsed();
            let handle_id = server.next_id();
            // Counts only: listing names and IDs would enumerate the devices
            // three times per device on Windows.
            let recording_count = audio.recording_device_count() as i32;
            let playout_count = audio.playout_device_count() as i32;
            let counted = started.elapsed();

            let info = proto::PlatformAudioInfo {
                recording_device_count: recording_count,
                playout_device_count: playout_count,
            };

            let rootapp = RootAudioSession::start(server, handle_id, audio.clone());
            server.store_handle(handle_id, FfiPlatformAudio { audio, rootapp });
            let total = started.elapsed();
            let level = if total >= SLOW_CREATE { log::Level::Warn } else { log::Level::Info };
            log::log!(
                level,
                "[PLATFORM_AUDIO_FFI] PlatformAudio created in {} ms (PlatformAudio::new {} ms, \
                 device counts {} ms, session {} ms): handle_id={}, recording_devices={}, \
                 playout_devices={}",
                total.as_millis(),
                created.as_millis(),
                counted.saturating_sub(created).as_millis(),
                total.saturating_sub(counted).as_millis(),
                handle_id,
                recording_count,
                playout_count
            );

            Ok(proto::NewPlatformAudioResponse {
                message: Some(proto::new_platform_audio_response::Message::PlatformAudio(
                    proto::OwnedPlatformAudio {
                        handle: proto::FfiOwnedHandle { id: handle_id },
                        info,
                    },
                )),
            })
        }
        Err(e) => {
            log::error!(
                "[PLATFORM_AUDIO_FFI] PlatformAudio::new() failed after {} ms: {}. This typically means: \
                (1) Android JNI not initialized (init_android not called), \
                (2) RECORD_AUDIO permission not granted, \
                (3) No audio devices available, or \
                (4) Another app has exclusive audio focus.",
                started.elapsed().as_millis(),
                e
            );
            Ok(proto::NewPlatformAudioResponse {
                message: Some(proto::new_platform_audio_response::Message::Error(e.to_string())),
            })
        }
    }
}

pub fn on_get_audio_devices(
    server: &'static FfiServer,
    req: proto::GetAudioDevicesRequest,
) -> FfiResult<proto::GetAudioDevicesResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;

    // Use iterator-based device enumeration
    let playout_devices: Vec<_> = audio
        .playout_devices()
        .map(|device| proto::AudioDeviceInfo {
            index: device.index as u32,
            name: device.name,
            guid: Some(device.id.to_string()),
        })
        .collect();

    let recording_devices: Vec<_> = audio
        .recording_devices()
        .map(|device| proto::AudioDeviceInfo {
            index: device.index as u32,
            name: device.name,
            guid: Some(device.id.to_string()),
        })
        .collect();

    Ok(proto::GetAudioDevicesResponse { playout_devices, recording_devices, error: None })
}

pub fn on_set_recording_device(
    server: &'static FfiServer,
    req: proto::SetRecordingDeviceRequest,
) -> FfiResult<proto::SetRecordingDeviceResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;

    let device_id = RecordingDeviceId::from_unchecked_guid(&req.device_id);
    match audio.set_recording_device(&device_id) {
        Ok(()) => Ok(proto::SetRecordingDeviceResponse { error: None }),
        Err(e) => Ok(proto::SetRecordingDeviceResponse { error: Some(e.to_string()) }),
    }
}

pub fn on_set_playout_device(
    server: &'static FfiServer,
    req: proto::SetPlayoutDeviceRequest,
) -> FfiResult<proto::SetPlayoutDeviceResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;

    let device_id = PlayoutDeviceId::from_unchecked_guid(&req.device_id);
    match audio.set_playout_device(&device_id) {
        Ok(()) => Ok(proto::SetPlayoutDeviceResponse { error: None }),
        Err(e) => Ok(proto::SetPlayoutDeviceResponse { error: Some(e.to_string()) }),
    }
}

pub fn on_start_recording(
    server: &'static FfiServer,
    req: proto::StartRecordingRequest,
) -> FfiResult<proto::StartRecordingResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;

    match audio.start_recording() {
        Ok(()) => Ok(proto::StartRecordingResponse { error: None }),
        Err(e) => Ok(proto::StartRecordingResponse { error: Some(e.to_string()) }),
    }
}

pub fn on_stop_recording(
    server: &'static FfiServer,
    req: proto::StopRecordingRequest,
) -> FfiResult<proto::StopRecordingResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;

    match audio.stop_recording() {
        Ok(()) => Ok(proto::StopRecordingResponse { error: None }),
        Err(e) => Ok(proto::StopRecordingResponse { error: Some(e.to_string()) }),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FFI_SERVER;

    /// Helper to get a static reference to FFI_SERVER for tests
    fn server() -> &'static FfiServer {
        &FFI_SERVER
    }

    #[test]
    fn test_new_platform_audio() {
        if std::env::var_os("LK_HW_AUDIO_TESTS").is_none() {
            println!("Skipping test_new_platform_audio: opens real audio devices; set LK_HW_AUDIO_TESTS=1");
            return;
        }
        let req = proto::NewPlatformAudioRequest {};
        let res = on_new_platform_audio(server(), req).unwrap();

        match res.message {
            Some(proto::new_platform_audio_response::Message::PlatformAudio(audio)) => {
                // Verify we got a valid handle
                assert!(audio.handle.id > 0);

                // Verify device counts are reasonable (>= 0)
                assert!(audio.info.recording_device_count >= 0);
                assert!(audio.info.playout_device_count >= 0);

                println!(
                    "PlatformAudio created: handle={}, recording_devices={}, playout_devices={}",
                    audio.handle.id,
                    audio.info.recording_device_count,
                    audio.info.playout_device_count
                );

                // Clean up - drop the handle
                server().drop_handle(audio.handle.id);
            }
            Some(proto::new_platform_audio_response::Message::Error(e)) => {
                println!("Skipping test_new_platform_audio - PlatformAudio unavailable: {}", e);
            }
            None => {
                panic!("Empty response");
            }
        }
    }
}
