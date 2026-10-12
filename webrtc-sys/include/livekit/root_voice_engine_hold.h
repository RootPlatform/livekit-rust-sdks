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

#include <atomic>
#include <memory>

#include "api/scoped_refptr.h"

namespace webrtc {
class ConnectionContext;
class Thread;
}  // namespace webrtc

namespace livekit_ffi {

// m150 initializes the voice engine (adm_helpers::Init, the APM options and
// the ADM's transport: AudioTransportImpl with the APM, the capture processor
// and the playout mixer) when the first PeerConnection takes a reference to
// the ConnectionContext, and terminates it with the last one. While platform
// audio is held this keeps a reference of its own, as a PeerConnection does,
// so the playout hold, the app's own start and the mic test monitor run
// through the APM and the mixer before the first call, between calls and
// after the last one.
class RootVoiceEngineHold {
 public:
  RootVoiceEngineHold(webrtc::Thread* worker_thread,
                      webrtc::scoped_refptr<webrtc::ConnectionContext> context);
  ~RootVoiceEngineHold();

  RootVoiceEngineHold(const RootVoiceEngineHold&) = delete;
  RootVoiceEngineHold& operator=(const RootVoiceEngineHold&) = delete;

  // One of each per platform reference, posted to the worker thread in call
  // order; neither waits.
  void Acquire();
  void Release();
  // Lets go of the engine on the worker thread once the posted calls ran;
  // later calls do nothing. The context's owner calls it before releasing the
  // context, so the context is never destroyed off its signaling thread.
  void Shutdown();

 private:
  struct State;
  webrtc::Thread* const worker_thread_;
  const std::shared_ptr<State> state_;
  std::atomic<bool> shut_down_{false};
};

}  // namespace livekit_ffi
