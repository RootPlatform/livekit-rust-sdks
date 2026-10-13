// Copyright 2026 LiveKit, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The RootApp echo model: DTLN-aec-128 (Westhausen 2021, MIT) as AEC3's
//! neural residual echo estimator. A TFLite reader and an allocation-free
//! executor for its two float graphs, the streaming model, and the 4 ms block
//! estimator AEC3 calls (estimator-spec.md). The weights are embedded
//! ([`weights`]).
//!
//! [`prepare`] decodes them once per process (off the audio threads); every
//! [`Estimator`] shares that [`Model`] and allocates its own state when it is
//! built. `Estimator::estimate` and `Estimator::reset` never allocate,
//! lock, log or panic.

use std::sync::{Arc, OnceLock};

pub mod dtln;
mod estimator;
#[allow(unsafe_code)]
mod kernels;
mod plan;
mod tflite;
pub mod weights;

pub use estimator::{
    Estimator, Hook, ModelFraction, Rule, TodayMasks, AEC3_BINS, AEC3_BLOCK, SCALE,
};
pub use kernels::Kernel;

use dtln::{BINS, BLOCK};
use plan::Plan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Malformed(&'static str),
    Unsupported(&'static str),
    /// The graphs are not DTLN-aec's (inputs or outputs of other sizes).
    NotDtlnAec(&'static str),
    KernelUnavailable,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Malformed(what) => write!(f, "malformed model: {what}"),
            Error::Unsupported(what) => write!(f, "unsupported model: {what}"),
            Error::NotDtlnAec(what) => write!(f, "not a DTLN-aec model: {what}"),
            Error::KernelUnavailable => write!(f, "this CPU cannot run the kernel"),
        }
    }
}

impl std::error::Error for Error {}

/// Both stages of DTLN-aec, parsed and validated, with the dot product kernel
/// they run with. Shared by every stream.
pub struct Model {
    pub(crate) stage1: Plan,
    pub(crate) stage2: Plan,
    /// Stage 1: mic magnitude, LSTM state, loopback magnitude.
    pub(crate) stage1_inputs: [usize; 3],
    /// Stage 1: mask, LSTM state.
    pub(crate) stage1_outputs: [usize; 2],
    /// Stage 2: masked estimate, LSTM state, loopback block.
    pub(crate) stage2_inputs: [usize; 3],
    /// Stage 2: output block, LSTM state.
    pub(crate) stage2_outputs: [usize; 2],
    pub(crate) kernel: Kernel,
}

impl Model {
    /// Parses both stages (`*_1.tflite`, `*_2.tflite`) of any DTLN-aec size
    /// and picks the fastest kernel the CPU runs.
    pub fn from_tflite(stage1: &[u8], stage2: &[u8]) -> Result<Model, Error> {
        let p1 = Plan::new(&tflite::load(stage1)?)?;
        let p2 = Plan::new(&tflite::load(stage2)?)?;
        let three = |p: &Plan| -> Result<[usize; 3], Error> {
            p.inputs.as_slice().try_into().map_err(|_| Error::NotDtlnAec("three inputs"))
        };
        let two = |p: &Plan| -> Result<[usize; 2], Error> {
            p.outputs.as_slice().try_into().map_err(|_| Error::NotDtlnAec("two outputs"))
        };
        let model = Model {
            stage1_inputs: three(&p1)?,
            stage1_outputs: two(&p1)?,
            stage2_inputs: three(&p2)?,
            stage2_outputs: two(&p2)?,
            stage1: p1,
            stage2: p2,
            kernel: Kernel::detect(),
        };
        model.check_sizes()?;
        Ok(model)
    }

    /// The embedded DTLN-aec-128.
    pub fn embedded() -> Result<Model, Error> {
        Model::from_tflite(weights::STAGE1, weights::STAGE2)
    }

    /// The same model running another kernel (tests and benchmarks).
    pub fn with_kernel(mut self, kernel: Kernel) -> Result<Model, Error> {
        if !kernel.available() {
            return Err(Error::KernelUnavailable);
        }
        self.kernel = kernel;
        Ok(self)
    }

    pub fn kernel(&self) -> Kernel {
        self.kernel
    }

    /// Weights of both stages.
    pub fn params(&self) -> usize {
        self.stage1.params + self.stage2.params
    }

    /// Multiply-accumulates of the fully connected layers per hop.
    pub fn macs(&self) -> usize {
        self.stage1.macs + self.stage2.macs
    }

    fn check_sizes(&self) -> Result<(), Error> {
        let s1 = self.stage1.new_state();
        let s2 = self.stage2.new_state();
        let len = |s: &plan::PlanState, t: usize| s.bufs.get(t).map_or(0, Vec::len);
        let [mic, state1, lpb] = self.stage1_inputs;
        let [mask, state1_out] = self.stage1_outputs;
        let [estimate, state2, block] = self.stage2_inputs;
        let [out, state2_out] = self.stage2_outputs;
        if len(&s1, mic) != BINS || len(&s1, lpb) != BINS || len(&s1, mask) != BINS {
            return Err(Error::NotDtlnAec("stage 1 is not 257 bins"));
        }
        if len(&s2, estimate) != BLOCK || len(&s2, block) != BLOCK || len(&s2, out) != BLOCK {
            return Err(Error::NotDtlnAec("stage 2 is not 512 samples"));
        }
        if len(&s1, state1) != len(&s1, state1_out) || len(&s2, state2) != len(&s2, state2_out) {
            return Err(Error::NotDtlnAec("LSTM state sizes"));
        }
        Ok(())
    }
}

static SHARED: OnceLock<Result<Arc<Model>, Error>> = OnceLock::new();

/// Decodes the embedded model once per process (about 10 ms plus reading
/// 7 MB of weights). Blocking; never call it on an audio thread. Returns
/// whether the model is ready.
pub fn prepare() -> bool {
    SHARED.get_or_init(|| Model::embedded().map(Arc::new)).is_ok()
}

/// Whether [`prepare`] finished successfully (an atomic load).
pub fn prepared() -> bool {
    matches!(SHARED.get(), Some(Ok(_)))
}

/// The prepared model, once [`prepare`] succeeded.
pub fn shared() -> Option<Arc<Model>> {
    match SHARED.get() {
        Some(Ok(model)) => Some(model.clone()),
        _ => None,
    }
}
