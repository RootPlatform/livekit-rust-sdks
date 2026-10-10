pub const PRE_ROLL_FRAMES: u32 = 10;
pub const SURPLUS_DRAIN_FRAMES: u32 = 100;
pub const FORCED_DRAIN_FRAMES: u32 = 500;
pub const QUIET_RMS: f32 = 0.01;

/// The ring a frame and backlog need: the backlog (at least two frames) plus four frames.
pub fn ring_samples(frame_samples: usize, max_backlog_samples: usize) -> usize {
    let frame = frame_samples.max(1);
    max_backlog_samples.max(frame.saturating_mul(2)).saturating_add(frame.saturating_mul(4))
}

/// Counters the latency is computed from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AlignerCounters {
    pub priming_samples: u64,
    pub padded_samples: u64,
    pub trimmed_samples: u64,
    pub drained_samples: u64,
}

impl AlignerCounters {
    pub fn inserted_samples(&self) -> u64 {
        self.priming_samples.saturating_add(self.padded_samples)
    }

    /// Managed `DenoiserStats.LatencyMs`: max(0, inserted − drained − trimmed) in ms at `rate`.
    pub fn latency_ms(&self, rate: u32) -> f64 {
        let removed = self.drained_samples.saturating_add(self.trimmed_samples);
        let samples = self.inserted_samples().saturating_sub(removed);
        samples as f64 * 1000.0 / f64::from(rate.max(1))
    }
}

/// Managed `DenoiseFrameAligner` (`EffectsSdk\DenoiseFrameAligner.cs`): turns a denoiser's
/// variable output back into fixed frames. The ring is allocated once (`capacity`); a smaller
/// frame or backlog uses part of it.
#[derive(Clone, Debug)]
pub struct DenoiseFrameAligner {
    ring: Vec<f32>,
    len: usize,
    frame_samples: usize,
    max_backlog: usize,
    head: usize,
    count: usize,
    pops_since_prime: u32,
    surplus_pops: u32,
    primed: bool,
    underruns: u64,
    output_samples: u64,
    denoised_samples: u64,
    lifetime: AlignerCounters,
    since_reset: AlignerCounters,
}

impl DenoiseFrameAligner {
    /// `capacity`: samples to allocate, so that `configure` can later take any frame and backlog
    /// whose ring fits; never less than this frame and backlog need.
    pub fn new(frame_samples: usize, max_backlog_samples: usize, capacity: usize) -> Self {
        let needed = ring_samples(frame_samples, max_backlog_samples);
        let mut aligner = Self {
            ring: vec![0.0; capacity.max(needed)],
            len: 0,
            frame_samples: 1,
            max_backlog: 2,
            head: 0,
            count: 0,
            pops_since_prime: 0,
            surplus_pops: 0,
            primed: false,
            underruns: 0,
            output_samples: 0,
            denoised_samples: 0,
            lifetime: AlignerCounters::default(),
            since_reset: AlignerCounters::default(),
        };
        aligner.configure(frame_samples, max_backlog_samples);
        aligner
    }

    /// A new frame and backlog within the allocation (reset). Returns false, keeping the old
    /// geometry, when they do not fit.
    pub fn configure(&mut self, frame_samples: usize, max_backlog_samples: usize) -> bool {
        let frame = frame_samples.max(1);
        let needed = ring_samples(frame, max_backlog_samples);
        if needed > self.ring.len() {
            return false;
        }
        self.frame_samples = frame;
        self.max_backlog = max_backlog_samples.max(frame.saturating_mul(2));
        self.len = needed;
        self.reset();
        true
    }

    pub fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    pub fn max_backlog_samples(&self) -> usize {
        self.max_backlog
    }

    pub fn is_primed(&self) -> bool {
        self.primed
    }

    pub fn buffered_samples(&self) -> usize {
        self.count
    }

    pub fn priming_samples(&self) -> u64 {
        self.lifetime.priming_samples
    }

    pub fn padded_samples(&self) -> u64 {
        self.lifetime.padded_samples
    }

    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    pub fn trimmed_samples(&self) -> u64 {
        self.lifetime.trimmed_samples
    }

    pub fn drained_samples(&self) -> u64 {
        self.lifetime.drained_samples
    }

    pub fn output_samples(&self) -> u64 {
        self.output_samples
    }

    pub fn denoised_samples(&self) -> u64 {
        self.denoised_samples
    }

    pub fn inserted_samples(&self) -> u64 {
        self.lifetime.inserted_samples()
    }

    /// Cumulative over the aligner's life (managed: never reset).
    pub fn lifetime(&self) -> AlignerCounters {
        self.lifetime
    }

    /// Since the last `reset` (or `configure`).
    pub fn since_reset(&self) -> AlignerCounters {
        self.since_reset
    }

    pub fn push(&mut self, samples: &[f32]) {
        let mut rest = samples;
        while !rest.is_empty() {
            let free = self.len.saturating_sub(self.count);
            if free == 0 {
                if self.count == 0 {
                    return;
                }
                self.trim(self.count.min(rest.len()));
                continue;
            }
            let n = free.min(rest.len());
            let tail = self.wrap(self.head.saturating_add(self.count));
            let first = n.min(self.len.saturating_sub(tail));
            let Some((now, later)) = rest.split_at_checked(n) else {
                return;
            };
            let Some((to_end, from_start)) = now.split_at_checked(first) else {
                return;
            };
            if let Some(dst) = self.ring.get_mut(tail..tail.saturating_add(first)) {
                dst.copy_from_slice(to_end);
            }
            if let Some(dst) = self.ring.get_mut(..from_start.len()) {
                dst.copy_from_slice(from_start);
            }
            self.count = self.count.saturating_add(n);
            rest = later;
        }
        if self.count > self.max_backlog {
            self.trim(self.count.saturating_sub(self.frame_samples));
        }
    }

    /// Writes the next frame. A frame of the wrong length is zero-filled and counts nothing
    /// (managed throws); returns whether it was the right length.
    pub fn pop(&mut self, frame: &mut [f32]) -> bool {
        if frame.len() != self.frame_samples {
            frame.fill(0.0);
            return false;
        }
        let len = frame.len() as u64;
        self.output_samples = self.output_samples.saturating_add(len);
        if !self.primed {
            if self.count < self.frame_samples {
                frame.fill(0.0);
                self.lifetime.priming_samples = self.lifetime.priming_samples.saturating_add(len);
                self.since_reset.priming_samples =
                    self.since_reset.priming_samples.saturating_add(len);
                return true;
            }
            self.primed = true;
        }

        self.drain_surplus();

        let take = self.count.min(frame.len());
        let Some((head, tail)) = frame.split_at_mut_checked(take) else {
            return true;
        };
        self.read(head);
        self.denoised_samples = self.denoised_samples.saturating_add(take as u64);
        if !tail.is_empty() {
            tail.fill(0.0);
            let padded = tail.len() as u64;
            self.lifetime.padded_samples = self.lifetime.padded_samples.saturating_add(padded);
            self.since_reset.padded_samples =
                self.since_reset.padded_samples.saturating_add(padded);
            self.underruns = self.underruns.saturating_add(1);
        }
        true
    }

    pub fn reset(&mut self) {
        self.head = 0;
        self.count = 0;
        self.primed = false;
        self.pops_since_prime = 0;
        self.surplus_pops = 0;
        self.since_reset = AlignerCounters::default();
    }

    /// Surplus = what would still be buffered after this pop. The oldest whole frames go.
    fn drain_surplus(&mut self) {
        let surplus_frames =
            self.count.checked_div(self.frame_samples).unwrap_or(0).saturating_sub(1);
        if self.pops_since_prime < PRE_ROLL_FRAMES {
            self.pops_since_prime = self.pops_since_prime.saturating_add(1);
            if surplus_frames > 0 {
                self.drain(surplus_frames.saturating_mul(self.frame_samples));
            }
            self.surplus_pops = 0;
            return;
        }
        if surplus_frames == 0 {
            self.surplus_pops = 0;
            return;
        }
        self.surplus_pops = self.surplus_pops.saturating_add(1);
        if self.surplus_pops < SURPLUS_DRAIN_FRAMES {
            return;
        }
        if self.surplus_pops >= FORCED_DRAIN_FRAMES
            || self.oldest_frame_rms() < f64::from(QUIET_RMS)
        {
            self.drain(self.frame_samples);
            self.surplus_pops = 0;
        }
    }

    fn oldest_frame_rms(&self) -> f64 {
        let mut sum = 0.0f64;
        let (first, second) = self.segments(self.frame_samples);
        for &v in first.iter().chain(second.iter()) {
            sum += f64::from(v) * f64::from(v);
        }
        (sum / self.frame_samples as f64).sqrt()
    }

    /// The oldest `n` buffered samples as the part up to the ring's end and the wrapped part.
    fn segments(&self, n: usize) -> (&[f32], &[f32]) {
        let n = n.min(self.count);
        let first = n.min(self.len.saturating_sub(self.head));
        let a = self.ring.get(self.head..self.head.saturating_add(first)).unwrap_or(&[]);
        let b = self.ring.get(..n.saturating_sub(first)).unwrap_or(&[]);
        (a, b)
    }

    fn read(&mut self, destination: &mut [f32]) {
        let n = destination.len().min(self.count);
        let (a, b) = self.segments(n);
        let Some((to_a, rest)) = destination.split_at_mut_checked(a.len()) else {
            return;
        };
        to_a.copy_from_slice(a);
        if let Some(to_b) = rest.get_mut(..b.len()) {
            to_b.copy_from_slice(b);
        }
        self.skip(n);
    }

    fn drain(&mut self, samples: usize) {
        let samples = samples.min(self.count);
        self.skip(samples);
        let n = samples as u64;
        self.lifetime.drained_samples = self.lifetime.drained_samples.saturating_add(n);
        self.since_reset.drained_samples = self.since_reset.drained_samples.saturating_add(n);
    }

    fn trim(&mut self, samples: usize) {
        let samples = samples.min(self.count);
        self.skip(samples);
        let n = samples as u64;
        self.lifetime.trimmed_samples = self.lifetime.trimmed_samples.saturating_add(n);
        self.since_reset.trimmed_samples = self.since_reset.trimmed_samples.saturating_add(n);
    }

    fn skip(&mut self, samples: usize) {
        self.head = self.wrap(self.head.saturating_add(samples));
        self.count = self.count.saturating_sub(samples);
    }

    fn wrap(&self, position: usize) -> usize {
        position.checked_rem(self.len).unwrap_or(0)
    }
}
