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

#ifndef WEBRTC_MF_DEFERRED_RELEASE_H_
#define WEBRTC_MF_DEFERRED_RELEASE_H_

#include <chrono>
#include <condition_variable>
#include <deque>
#include <functional>
#include <mutex>
#include <thread>
#include <utility>

namespace livekit_ffi {

// Runs teardown work in order on one worker thread, started by the first
// Post(), so the caller does not wait for it. The worker is detached and runs
// for the rest of the process, which is why the destructor is deleted: the
// queue lives on the heap and is never freed, and no static destructor joins
// a thread under the loader lock.
class DeferredReleaseQueue {
 public:
  // `thread_init` runs once on the worker thread before its first task.
  explicit DeferredReleaseQueue(std::function<void()> thread_init)
      : thread_init_(std::move(thread_init)) {}
  DeferredReleaseQueue(const DeferredReleaseQueue&) = delete;
  DeferredReleaseQueue& operator=(const DeferredReleaseQueue&) = delete;
  ~DeferredReleaseQueue() = delete;

  void Post(std::function<void()> task) {
    std::lock_guard<std::mutex> lock(mutex_);
    tasks_.push_back(std::move(task));
    if (!started_) {
      started_ = true;
      std::thread([this] { Run(); }).detach();
    }
    task_cv_.notify_one();
  }

  // True once every task posted so far has finished.
  bool WaitIdle(std::chrono::milliseconds timeout) {
    std::unique_lock<std::mutex> lock(mutex_);
    return idle_cv_.wait_for(lock, timeout,
                             [this] { return tasks_.empty() && !running_; });
  }

 private:
  void Run() {
    if (thread_init_) {
      thread_init_();
    }
    std::unique_lock<std::mutex> lock(mutex_);
    for (;;) {
      task_cv_.wait(lock, [this] { return !tasks_.empty(); });
      std::function<void()> task = std::move(tasks_.front());
      tasks_.pop_front();
      running_ = true;
      lock.unlock();
      task();
      task = nullptr;
      lock.lock();
      running_ = false;
      if (tasks_.empty()) {
        idle_cv_.notify_all();
      }
    }
  }

  const std::function<void()> thread_init_;
  std::mutex mutex_;
  std::condition_variable task_cv_;
  std::condition_variable idle_cv_;
  std::deque<std::function<void()>> tasks_;
  bool running_ = false;
  bool started_ = false;
};

}  // namespace livekit_ffi

#endif  // WEBRTC_MF_DEFERRED_RELEASE_H_
