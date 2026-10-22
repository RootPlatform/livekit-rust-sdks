/*
 * Copyright 2026 LiveKit, Inc.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#pragma once

#include <memory>
#include <string>

#include "livekit/adm_proxy.h"

namespace livekit_ffi {

#if defined(WEBRTC_WIN)
// Repeats AudioDeviceWindowsCore::InitPlayout (m150) on `endpoint_id` (empty:
// the default console output) with a fresh client it never starts: the
// 16-bit PCM search over 48000, 44100, 16000, 96000, 32000 and 8000 Hz in
// `stereo`'s channel order, then Initialize in shared, event-driven mode
// without conversion, SetEventHandle and GetService. Reports the step that
// failed, its HRESULT, the format asked for and the mix format, and whether a
// client the audio engine converts for opens. Runs on a COM MTA thread.
AdmPlayoutDiagnosis DiagnoseWasapiPlayout(const std::string& endpoint_id,
                                          bool stereo);

// A shared, event-driven WASAPI render stream of 48 kHz int16 that the audio
// engine converts to the device's mix format (AUTOCONVERTPCM), pulling 10 ms
// at a time through `transport` on an MMCSS "Pro Audio" thread.
std::unique_ptr<AdmPlayoutFallback> CreateWasapiPlayoutFallback(
    webrtc::AudioTransport* transport);
#endif

}  // namespace livekit_ffi
