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

// Standalone unit test for the deferred release queue that MediaFoundation
// encoder and decoder teardown runs on. It depends on nothing but the C++
// standard library:
//
//   cl /std:c++20 /EHsc /I ..\src\mf mf_deferred_release_test.cc
//   c++ -std=c++17 -pthread -I ../src/mf mf_deferred_release_test.cc -o mf_deferred_release_test

#include "mf_deferred_release.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <mutex>
#include <thread>
#include <vector>

namespace {

using Clock = std::chrono::steady_clock;
using std::chrono::milliseconds;

int g_failures = 0;

void Expect(bool condition, const char* what) {
  if (!condition) {
    std::fprintf(stderr, "FAIL: %s\n", what);
    ++g_failures;
  }
}

livekit_ffi::DeferredReleaseQueue* NewQueue(std::function<void()> init = {}) {
  return new livekit_ffi::DeferredReleaseQueue(std::move(init));
}

void TestPostDoesNotWaitForTheTask() {
  livekit_ffi::DeferredReleaseQueue* queue = NewQueue();
  const Clock::time_point start = Clock::now();
  queue->Post([] { std::this_thread::sleep_for(milliseconds(200)); });
  Expect(Clock::now() - start < milliseconds(20),
         "Post of a 200 ms task returns within 20 ms");
  Expect(!queue->WaitIdle(milliseconds(50)),
         "WaitIdle times out while the task runs");
  Expect(queue->WaitIdle(milliseconds(1000)),
         "WaitIdle succeeds once the task finishes");
  Expect(Clock::now() - start >= milliseconds(200),
         "WaitIdle returned no earlier than the task's end");
}

void TestIdleBeforeAnyPost() {
  livekit_ffi::DeferredReleaseQueue* queue = NewQueue();
  const Clock::time_point start = Clock::now();
  Expect(queue->WaitIdle(milliseconds(1000)), "an unused queue is idle");
  Expect(Clock::now() - start < milliseconds(20),
         "an unused queue reports idle at once");
}

void TestTasksRunInOrder() {
  livekit_ffi::DeferredReleaseQueue* queue = NewQueue();
  std::mutex mutex;
  std::vector<int> order;
  for (int i = 0; i < 50; i++) {
    queue->Post([&mutex, &order, i] {
      if (i == 0) {
        std::this_thread::sleep_for(milliseconds(20));
      }
      std::lock_guard<std::mutex> lock(mutex);
      order.push_back(i);
    });
  }
  Expect(queue->WaitIdle(milliseconds(2000)), "queue drains");
  bool in_order = order.size() == 50;
  for (size_t i = 0; in_order && i < order.size(); i++) {
    in_order = order[i] == static_cast<int>(i);
  }
  Expect(in_order, "tasks run in FIFO order");
}

void TestWaitIdleCoversTasksPostedWhileBusy() {
  livekit_ffi::DeferredReleaseQueue* queue = NewQueue();
  std::atomic<int> done{0};
  queue->Post([&done] {
    std::this_thread::sleep_for(milliseconds(50));
    done++;
  });
  queue->Post([&done] {
    std::this_thread::sleep_for(milliseconds(50));
    done++;
  });
  Expect(queue->WaitIdle(milliseconds(1000)), "queue drains");
  Expect(done.load() == 2, "WaitIdle waits for every posted task");
}

void TestInitHookRunsOnceOnTheWorker() {
  std::atomic<int> init_calls{0};
  std::thread::id init_thread;
  livekit_ffi::DeferredReleaseQueue* queue =
      NewQueue([&init_calls, &init_thread] {
        init_calls++;
        init_thread = std::this_thread::get_id();
      });
  std::thread::id task_thread;
  for (int i = 0; i < 5; i++) {
    queue->Post([&task_thread] { task_thread = std::this_thread::get_id(); });
    Expect(queue->WaitIdle(milliseconds(1000)), "queue drains");
  }
  Expect(init_calls.load() == 1, "init hook runs exactly once");
  Expect(init_thread == task_thread, "init hook runs on the worker thread");
  Expect(init_thread != std::this_thread::get_id(),
         "worker is not the posting thread");
}

}  // namespace

int main() {
  TestPostDoesNotWaitForTheTask();
  TestIdleBeforeAnyPost();
  TestTasksRunInOrder();
  TestWaitIdleCoversTasksPostedWhileBusy();
  TestInitHookRunsOnceOnTheWorker();

  if (g_failures != 0) {
    std::fprintf(stderr, "%d test(s) failed\n", g_failures);
    return EXIT_FAILURE;
  }
  std::printf("All mf_deferred_release tests passed\n");
  return EXIT_SUCCESS;
}
