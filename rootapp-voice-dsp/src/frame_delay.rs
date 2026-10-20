/// What a delayed frame carries besides its samples, as of when it went in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameTag {
    /// The startup hold zeroed it.
    pub held: bool,
    /// The transmit gate was open (not muted, push-to-talk not released).
    pub open: bool,
    /// Steady confirmed speech above QUIET_RMS (speech_gate::STEADY_SPEECH_FRAMES): the level
    /// gate shutting on it cuts speech.
    pub speech: bool,
}

/// A delay line of whole frames for the speech gate's look-ahead: `exchange` puts this frame in and
/// takes out the one `frames` frames older. Allocated once for `max_frames` frames of `max_frame`
/// samples; a delay of 0 holds nothing.
#[derive(Clone, Debug)]
pub struct FrameDelay {
    ring: Vec<f32>,
    tags: Vec<FrameTag>,
    max_frame: usize,
    frames: usize,
    len: usize,
    next: usize,
}

impl FrameDelay {
    pub fn new(max_frames: usize, max_frame: usize) -> Self {
        let max_frame = max_frame.max(1);
        Self {
            ring: vec![0.0; max_frames.saturating_mul(max_frame)],
            tags: vec![FrameTag::default(); max_frames],
            max_frame,
            frames: 0,
            len: 0,
            next: 0,
        }
    }

    pub fn max_frames(&self) -> usize {
        self.tags.len()
    }

    /// Frames in flight (0: inactive).
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The frame length the delay holds.
    pub fn frame_len(&self) -> usize {
        self.len
    }

    /// Drops whatever is in flight; the delay is inactive until the next `restart`.
    pub fn clear(&mut self) {
        self.frames = 0;
        self.next = 0;
    }

    /// Starts over at `frames` frames (clamped to the allocation; 0 clears) of `len` samples:
    /// every slot is silence tagged `tag`, so the first `frames` frames out are that silence.
    /// Returns the delay set.
    pub fn restart(&mut self, frames: usize, len: usize, tag: FrameTag) -> usize {
        let frames = frames.min(self.max_frames());
        if frames == 0 || len == 0 || len > self.max_frame {
            self.clear();
            return 0;
        }
        self.frames = frames;
        self.len = len;
        self.next = 0;
        if let Some(slots) = self.ring.get_mut(..frames.saturating_mul(self.max_frame)) {
            slots.fill(0.0);
        }
        if let Some(tags) = self.tags.get_mut(..frames) {
            tags.fill(tag);
        }
        frames
    }

    /// Writes the frame `frames` frames older than `current` into `out` and keeps `current` (with
    /// `tag`) in its place; returns the older frame's tag. Inactive, or a length other than the
    /// delay's: `out` gets as much of `current` as fits and the tag comes back as it went in.
    pub fn exchange(&mut self, current: &[f32], out: &mut [f32], tag: FrameTag) -> FrameTag {
        let len = current.len();
        let start = self.next.saturating_mul(self.max_frame);
        let slot = self.ring.get_mut(start..start.saturating_add(len));
        let held_tag = self.tags.get_mut(self.next);
        match (self.frames > 0 && len == self.len && out.len() == len, slot, held_tag) {
            (true, Some(slot), Some(held_tag)) => {
                out.copy_from_slice(slot);
                slot.copy_from_slice(current);
                let older = std::mem::replace(held_tag, tag);
                self.next = self.next.saturating_add(1).checked_rem(self.frames).unwrap_or(0);
                older
            }
            _ => {
                let n = len.min(out.len());
                if let (Some(out), Some(current)) = (out.get_mut(..n), current.get(..n)) {
                    out.copy_from_slice(current);
                }
                tag
            }
        }
    }
}
