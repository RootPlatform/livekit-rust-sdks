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

#include "livekit/root_voice_engine_hold.h"

#include <memory>
#include <utility>

#include "pc/connection_context.h"
#include "rtc_base/thread.h"

namespace livekit_ffi {

// Worker thread only, after the constructor.
struct RootVoiceEngineHold::State {
  webrtc::scoped_refptr<webrtc::ConnectionContext> context;
  int count = 0;
  std::unique_ptr<webrtc::ConnectionContext::MediaEngineReference> reference;
};

RootVoiceEngineHold::RootVoiceEngineHold(
    webrtc::Thread* worker_thread,
    webrtc::scoped_refptr<webrtc::ConnectionContext> context)
    : worker_thread_(worker_thread), state_(std::make_shared<State>()) {
  state_->context = std::move(context);
}

RootVoiceEngineHold::~RootVoiceEngineHold() {
  Shutdown();
}

void RootVoiceEngineHold::Acquire() {
  if (shut_down_.load(std::memory_order_acquire)) {
    return;
  }
  worker_thread_->PostTask([state = state_] {
    if (state->context && ++state->count == 1) {
      state->reference = std::make_unique<
          webrtc::ConnectionContext::MediaEngineReference>(state->context);
    }
  });
}

void RootVoiceEngineHold::Release() {
  if (shut_down_.load(std::memory_order_acquire)) {
    return;
  }
  worker_thread_->PostTask([state = state_] {
    if (state->context && state->count > 0 && --state->count == 0) {
      state->reference = nullptr;
    }
  });
}

void RootVoiceEngineHold::Shutdown() {
  if (shut_down_.exchange(true, std::memory_order_acq_rel)) {
    return;
  }
  worker_thread_->BlockingCall([state = state_] {
    state->reference = nullptr;
    state->count = 0;
    state->context = nullptr;
  });
}

}  // namespace livekit_ffi
