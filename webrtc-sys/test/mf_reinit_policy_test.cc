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

// Standalone, hardware-free unit test for the MediaFoundation encoder re-init
// policy. It depends on nothing but the C++ standard library:
//
//   cl /std:c++20 /EHsc /I ..\src\mf mf_reinit_policy_test.cc
//   c++ -std=c++17 -I ../src/mf mf_reinit_policy_test.cc -o mf_reinit_policy_test

#include "mf_reinit_policy.h"

#include <cstdio>
#include <cstdlib>

namespace {

int g_failures = 0;

void Expect(bool condition, const char* what) {
  if (!condition) {
    std::fprintf(stderr, "FAIL: %s\n", what);
    ++g_failures;
  }
}

using livekit_ffi::EncoderReinitPolicy;

constexpr int64_t kFrameMs = 33;

// Encodes `seconds` of 30 fps frames of `frame_bits` each, starting at
// `*now_ms`, asking the policy about `target_bps` before every frame as the
// encoder does. Returns whether it asked for a re-init.
bool Run(EncoderReinitPolicy& policy,
         uint32_t target_bps,
         uint64_t frame_bits,
         int seconds,
         int64_t* now_ms) {
  for (int i = 0; i < seconds * 30; i++) {
    if (policy.ShouldReinit(target_bps, *now_ms)) {
      return true;
    }
    policy.OnEncoded(frame_bits / 8, *now_ms);
    *now_ms += kFrameMs;
  }
  return false;
}

void TestPlaceholderStartRebuildsAtFirstRealTarget() {
  EncoderReinitPolicy policy;
  policy.OnConfigured(30'000, 3'000, 0);
  Expect(!policy.ShouldReinit(30'000, 10), "placeholder at its own target");
  Expect(!policy.ShouldReinit(140'000, 20), "placeholder below 150 kbps");
  Expect(policy.ShouldReinit(4'000'000, 30),
         "placeholder rebuilds immediately for a real target");
}

void TestPausedPlaceholderLayerRebuildsBeforeItsFirstFrame() {
  EncoderReinitPolicy policy;
  policy.OnConfigured(49'000, 4'900, 0);
  Expect(policy.ShouldReinit(1'500'000, 2'000),
         "a 1080p top layer started at 49 kbps rebuilds with no frames yet");
}

void TestTargetWithinTwiceConfiguredNeverRebuilds() {
  EncoderReinitPolicy policy;
  policy.OnConfigured(2'000'000, 200'000, 0);
  int64_t now = 0;
  Expect(!Run(policy, 3'900'000, 200'000, 10, &now),
         "below 2x the configured bitrate, even with full frames");
}

void TestStarvedEncoderRebuildsAfterCooldown() {
  EncoderReinitPolicy policy;
  policy.OnConfigured(1'000'000, 100'000, 0);
  int64_t now = 0;
  Expect(!Run(policy, 5'000'000, 95'000, 4, &now),
         "no rebuild inside the 5 s cooldown");
  Expect(Run(policy, 5'000'000, 95'000, 3, &now),
         "frames at the VBV cap with a 5x target rebuild after the cooldown");
  Expect(now >= EncoderReinitPolicy::kCooldownMs, "not before the cooldown");
}

void TestTargetMustStayRaisedForASecond() {
  EncoderReinitPolicy policy;
  policy.OnConfigured(1'000'000, 100'000, 0);
  int64_t now = 0;
  Expect(!Run(policy, 1'000'000, 95'000, 6, &now), "settled at the start");
  Expect(!policy.ShouldReinit(5'000'000, now), "a raise is not yet sustained");
  Run(policy, 1'000'000, 95'000, 1, &now);
  Expect(!policy.ShouldReinit(5'000'000, now),
         "a dip below 2x restarts the sustain timer");
  Expect(Run(policy, 5'000'000, 95'000, 2, &now),
         "a raise held for a second rebuilds");
}

void TestEncoderMeetingARaisedTargetDoesNotRebuild() {
  EncoderReinitPolicy at_2x;
  at_2x.OnConfigured(1'000'000, 100'000, 0);
  int64_t now = 0;
  Expect(!Run(at_2x, 2'000'000, 2'000'000 / 30, 20, &now),
         "frames of target/fps at 2x the configured rate are rate-following");

  EncoderReinitPolicy at_3x;
  at_3x.OnConfigured(1'000'000, 100'000, 0);
  now = 0;
  Expect(!Run(at_3x, 3'000'000, 3'000'000 / 30, 20, &now),
         "frames of target/fps at 3x the configured rate are rate-following");
}

void TestStaticOrEasyContentDoesNotRebuild() {
  EncoderReinitPolicy policy;
  policy.OnConfigured(1'000'000, 100'000, 0);
  int64_t now = 0;
  Expect(!Run(policy, 6'000'000, 20'000, 10, &now),
         "small frames mean the VBV is not what limits the encoder");

  EncoderReinitPolicy sparse;
  sparse.OnConfigured(1'000'000, 100'000, 0);
  int64_t t = 0;
  bool rebuilt = false;
  for (int i = 0; i < 20 && !rebuilt; i++) {
    rebuilt = sparse.ShouldReinit(6'000'000, t);
    sparse.OnEncoded(100'000 / 8, t);
    t += 1000;
  }
  Expect(!rebuilt, "one repeat frame a second is too few to judge");
}

void TestStaleWindowIsIgnored() {
  EncoderReinitPolicy policy;
  policy.OnConfigured(1'000'000, 100'000, 0);
  int64_t now = 0;
  Run(policy, 1'000'000, 95'000, 2, &now);
  now += 10'000;
  policy.ShouldReinit(5'000'000, now);
  now += 1'500;
  Expect(!policy.ShouldReinit(5'000'000, now),
         "frames from before a pause do not count");
}

void TestReconfigureResetsState() {
  EncoderReinitPolicy policy;
  policy.OnConfigured(1'000'000, 100'000, 0);
  int64_t now = 0;
  Expect(Run(policy, 5'000'000, 95'000, 8, &now), "first rebuild");
  policy.OnConfigured(5'000'000, 500'000, now);
  Expect(policy.configured_bps() == 5'000'000, "configured bitrate updated");
  Expect(!Run(policy, 5'000'000, 450'000, 8, &now),
         "no further rebuild at the new size");
}

}  // namespace

int main() {
  TestPlaceholderStartRebuildsAtFirstRealTarget();
  TestPausedPlaceholderLayerRebuildsBeforeItsFirstFrame();
  TestTargetWithinTwiceConfiguredNeverRebuilds();
  TestStarvedEncoderRebuildsAfterCooldown();
  TestTargetMustStayRaisedForASecond();
  TestEncoderMeetingARaisedTargetDoesNotRebuild();
  TestStaticOrEasyContentDoesNotRebuild();
  TestStaleWindowIsIgnored();
  TestReconfigureResetsState();

  if (g_failures != 0) {
    std::fprintf(stderr, "%d test(s) failed\n", g_failures);
    return EXIT_FAILURE;
  }
  std::printf("All mf_reinit_policy tests passed\n");
  return EXIT_SUCCESS;
}
