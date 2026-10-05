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

// Standalone unit test for the MediaFoundation encoder session count, which
// keeps a session counted until its transform's delayed final release. It
// depends on nothing but the C++ standard library:
//
//   cl /std:c++20 /EHsc /I ..\src\mf mf_encoder_sessions_test.cc
//   c++ -std=c++17 -pthread -I ../src/mf mf_encoder_sessions_test.cc -o mf_encoder_sessions_test

#include "mf_encoder_sessions.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <thread>

#include "mf_deferred_release.h"

namespace {

using Clock = std::chrono::steady_clock;
using livekit_ffi::EncoderSessionCount;
using std::chrono::milliseconds;

int g_failures = 0;

void Expect(bool condition, const char* what) {
  if (!condition) {
    std::fprintf(stderr, "FAIL: %s\n", what);
    ++g_failures;
  }
}

// Polls like the encoder's Encode() does while its session waits.
bool WaitUntilNoneClosing(EncoderSessionCount& sessions,
                          milliseconds timeout) {
  const Clock::time_point deadline = Clock::now() + timeout;
  while (sessions.Get().closing > 0) {
    if (Clock::now() >= deadline) {
      return false;
    }
    std::this_thread::sleep_for(milliseconds(5));
  }
  return true;
}

void TestClosingSessionsStayOpenUntilTheirFinalRelease() {
  EncoderSessionCount sessions;
  sessions.Open();
  sessions.Open();
  EncoderSessionCount::Counts counts = sessions.Open();
  Expect(counts.open == 3 && counts.closing == 0, "three sessions open");

  counts = sessions.BeginClose();
  Expect(counts.open == 3 && counts.closing == 1,
         "a closing session still counts as open");

  counts = sessions.FinishClose();
  Expect(counts.open == 2 && counts.closing == 0,
         "the final release closes the session");

  counts = sessions.Close();
  Expect(counts.open == 1 && counts.closing == 0,
         "a session without a delayed release closes at once");
  counts = sessions.Get();
  Expect(counts.open == 1 && counts.closing == 0, "Get reports the counts");
}

// The encoder's flow: the shut-down transform's final release, and with it
// the end of its session, runs on the delayed release queue, and the session
// stays counted (keeping a session limit reached) until then.
void TestSessionClosesWithTheDelayedFinalRelease() {
  EncoderSessionCount sessions;
  auto* delayed = new livekit_ffi::DelayedReleaseQueue({});
  constexpr int kMaxSessions = 2;
  sessions.Open();
  sessions.Open();

  sessions.BeginClose();
  const Clock::time_point closed_at = Clock::now();
  delayed->Post(milliseconds(200), [&sessions] { sessions.FinishClose(); });

  std::this_thread::sleep_for(milliseconds(100));
  Expect(sessions.Get().open >= kMaxSessions,
         "the closing session keeps the limit reached before its release");
  Expect(sessions.Get().closing == 1, "one session is closing");
  Expect(WaitUntilNoneClosing(sessions, milliseconds(2000)),
         "the closing session ends with the final release");
  const Clock::duration waited = Clock::now() - closed_at;
  Expect(waited >= milliseconds(190), "it stayed open until the release");
  Expect(waited < milliseconds(1000), "it closed soon after the release");
  Expect(sessions.Get().open < kMaxSessions,
         "a session can open after the final release");
}

void TestEveryClosingSessionEndsWithItsRelease() {
  EncoderSessionCount sessions;
  auto* delayed = new livekit_ffi::DelayedReleaseQueue({});
  std::atomic<int> released{0};
  for (int i = 0; i < 3; i++) {
    sessions.Open();
    sessions.BeginClose();
    delayed->Post(milliseconds(50 + 50 * i), [&sessions, &released] {
      released++;
      sessions.FinishClose();
    });
  }
  Expect(sessions.Get().closing == 3, "three sessions closing");
  Expect(WaitUntilNoneClosing(sessions, milliseconds(2000)),
         "every closing session ends");
  Expect(released.load() == 3, "after every final release");
  Expect(sessions.Get().open == 0, "no session is left open");
}

}  // namespace

int main() {
  TestClosingSessionsStayOpenUntilTheirFinalRelease();
  TestSessionClosesWithTheDelayedFinalRelease();
  TestEveryClosingSessionEndsWithItsRelease();

  if (g_failures != 0) {
    std::fprintf(stderr, "%d test(s) failed\n", g_failures);
    return EXIT_FAILURE;
  }
  std::printf("All mf_encoder_sessions tests passed\n");
  return EXIT_SUCCESS;
}
