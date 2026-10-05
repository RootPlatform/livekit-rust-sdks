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

#ifndef WEBRTC_MF_ENCODER_SESSIONS_H_
#define WEBRTC_MF_ENCODER_SESSIONS_H_

#include <chrono>
#include <condition_variable>
#include <mutex>

namespace livekit_ffi {

// Hardware encoder sessions, counted from a configured transform to its final
// release. An asynchronous MFT is shut down at once but its last references
// are released later on the DelayedReleaseQueue, and the driver may keep the
// hardware session until then, so a closing session still counts as open.
class EncoderSessionCount {
 public:
  struct Counts {
    int open = 0;
    int closing = 0;
  };

  Counts Open() {
    std::lock_guard<std::mutex> lock(mutex_);
    ++counts_.open;
    return counts_;
  }

  // The session's transform is shut down; FinishClose follows its final
  // release.
  Counts BeginClose() {
    std::lock_guard<std::mutex> lock(mutex_);
    ++counts_.closing;
    return counts_;
  }

  Counts FinishClose() {
    std::lock_guard<std::mutex> lock(mutex_);
    --counts_.closing;
    --counts_.open;
    closed_.notify_all();
    return counts_;
  }

  // A session whose transform needs no delayed release.
  Counts Close() {
    std::lock_guard<std::mutex> lock(mutex_);
    --counts_.open;
    return counts_;
  }

  Counts Get() {
    std::lock_guard<std::mutex> lock(mutex_);
    return counts_;
  }

  // True once no session is closing, false when `timeout` passes first.
  bool WaitForClosing(std::chrono::milliseconds timeout) {
    std::unique_lock<std::mutex> lock(mutex_);
    return closed_.wait_for(lock, timeout,
                            [this] { return counts_.closing == 0; });
  }

 private:
  std::mutex mutex_;
  std::condition_variable closed_;
  Counts counts_;
};

}  // namespace livekit_ffi

#endif  // WEBRTC_MF_ENCODER_SESSIONS_H_
