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

//! An allocation-free executor for the float TFLite graphs of DTLN-aec. What
//! depends only on shapes (reshape, slice, pack, unpack, concat, split)
//! becomes a copy map when the plan is built. The plan and its weights are
//! shared; each [`PlanState`] holds one stream's activations and LSTM states.
//! Every index the runner uses is checked when the plan is built, so running
//! it cannot go out of bounds.

use crate::kernels::Kernel;
use crate::tflite::{self, Graph};
use crate::Error;

#[derive(Clone, Copy)]
enum Unary {
    Square,
    Log,
    Rsqrt,
    Sqrt,
    Logistic,
    Tanh,
}

#[derive(Clone, Copy)]
enum Binary {
    Add,
    Sub,
    Mul,
    Div,
    SquaredDifference,
}

#[derive(Clone, Copy)]
enum Bcast {
    Same,
    ScalarB,
    ScalarA,
    LastB,
}

enum Step {
    Fc { x: usize, w: usize, b: Option<usize>, out: usize, rows: usize, cols: usize },
    Unary { kind: Unary, a: usize, out: usize },
    Binary { kind: Binary, a: usize, b: usize, out: usize, mode: Bcast },
    MeanLast { a: usize, out: usize, n: usize },
    Gather { srcs: Vec<usize>, map: Vec<(u32, u32)>, out: usize },
}

pub(crate) struct Plan {
    consts: Vec<Option<Box<[f32]>>>,
    sizes: Vec<usize>,
    steps: Vec<Step>,
    pub inputs: Vec<usize>,
    pub outputs: Vec<usize>,
    pub params: usize,
    pub macs: usize,
}

/// One stream's tensors (empty for the plan's constants).
pub(crate) struct PlanState {
    pub bufs: Vec<Vec<f32>>,
}

fn numel(s: &[usize]) -> usize {
    s.iter().product()
}

fn strides(s: &[usize]) -> Vec<usize> {
    let mut st = vec![1; s.len()];
    for i in (0..s.len().saturating_sub(1)).rev() {
        st[i] = st[i + 1] * s[i + 1];
    }
    st
}

fn index(i: i32, count: usize) -> Result<usize, Error> {
    usize::try_from(i).ok().filter(|&i| i < count).ok_or(Error::Malformed("tensor index"))
}

fn axis(value: i32, rank: usize) -> Result<usize, Error> {
    let rank = i32::try_from(rank).map_err(|_| Error::Malformed("rank"))?;
    if rank == 0 || value < -rank || value >= rank {
        return Err(Error::Malformed("axis"));
    }
    Ok(((value + rank) % rank) as usize)
}

impl Plan {
    pub fn new(g: &Graph) -> Result<Plan, Error> {
        let count = g.tensors.len();
        let shapes: Vec<Vec<usize>> = g
            .tensors
            .iter()
            .map(|t| t.shape.iter().map(|&d| usize::try_from(d.max(1)).unwrap_or(1)).collect())
            .collect();
        let sizes: Vec<usize> = shapes.iter().map(|s| numel(s)).collect();
        let mut consts: Vec<Option<Box<[f32]>>> = Vec::with_capacity(count);
        let mut params = 0;
        for (t, &size) in g.tensors.iter().zip(&sizes) {
            match t.data.as_ref().filter(|_| t.ty == tflite::FLOAT32) {
                Some(_) => {
                    let f = t.floats().ok_or(Error::Malformed("float tensor"))?;
                    if f.len() != size {
                        return Err(Error::Malformed("constant size"));
                    }
                    params += f.len();
                    consts.push(Some(f.into_boxed_slice()));
                }
                None => consts.push(None),
            }
        }
        let ints = |i: i32| -> Vec<i32> {
            usize::try_from(i)
                .ok()
                .and_then(|i| g.tensors.get(i))
                .and_then(|t| t.ints())
                .unwrap_or_default()
        };
        let mut steps = Vec::new();
        let mut macs = 0;
        for op in &g.ops {
            let ins: Vec<usize> = op
                .inputs
                .iter()
                .filter(|&&i| i >= 0)
                .map(|&i| index(i, count))
                .collect::<Result<_, _>>()?;
            let outs: Vec<usize> =
                op.outputs.iter().map(|&i| index(i, count)).collect::<Result<_, _>>()?;
            let (Some(&in0), Some(&out0)) = (ins.first(), outs.first()) else {
                return Err(Error::Malformed("operator without input or output"));
            };
            let a_shape = shapes[in0].clone();
            match op.code {
                tflite::FULLY_CONNECTED | tflite::CONV_2D => {
                    if op.opts.activation != 0 {
                        return Err(Error::Unsupported("fused activation"));
                    }
                    let w = *ins.get(1).ok_or(Error::Malformed("weights"))?;
                    let rows = *shapes[w].first().ok_or(Error::Malformed("weights"))?;
                    let cols = numel(shapes[w].get(1..).unwrap_or(&[]));
                    if sizes[in0] != cols || sizes[out0] != rows || sizes[w] != rows * cols {
                        return Err(Error::Malformed("fully connected shapes"));
                    }
                    let b = op
                        .inputs
                        .get(2)
                        .filter(|&&i| i >= 0)
                        .map(|&i| index(i, count))
                        .transpose()?;
                    if let Some(b) = b {
                        if sizes[b] != rows {
                            return Err(Error::Malformed("bias size"));
                        }
                    }
                    macs += rows * cols;
                    steps.push(Step::Fc { x: in0, w, b, out: out0, rows, cols });
                }
                tflite::SQUARE
                | tflite::LOG
                | tflite::RSQRT
                | tflite::SQRT
                | tflite::LOGISTIC
                | tflite::TANH => {
                    let kind = match op.code {
                        tflite::SQUARE => Unary::Square,
                        tflite::LOG => Unary::Log,
                        tflite::RSQRT => Unary::Rsqrt,
                        tflite::SQRT => Unary::Sqrt,
                        tflite::LOGISTIC => Unary::Logistic,
                        _ => Unary::Tanh,
                    };
                    if sizes[in0] != sizes[out0] {
                        return Err(Error::Malformed("unary sizes"));
                    }
                    steps.push(Step::Unary { kind, a: in0, out: out0 });
                }
                tflite::ADD
                | tflite::SUB
                | tflite::MUL
                | tflite::DIV
                | tflite::SQUARED_DIFFERENCE => {
                    if op.opts.activation != 0 {
                        return Err(Error::Unsupported("fused activation"));
                    }
                    let kind = match op.code {
                        tflite::ADD => Binary::Add,
                        tflite::SUB => Binary::Sub,
                        tflite::MUL => Binary::Mul,
                        tflite::DIV => Binary::Div,
                        _ => Binary::SquaredDifference,
                    };
                    let b = *ins.get(1).ok_or(Error::Malformed("binary operand"))?;
                    let (na, nb, no) = (sizes[in0], sizes[b], sizes[out0]);
                    let last = a_shape.last().copied().unwrap_or(1);
                    let mode = if na == nb && no == na {
                        Bcast::Same
                    } else if nb == 1 && no == na {
                        Bcast::ScalarB
                    } else if na == 1 && no == nb {
                        Bcast::ScalarA
                    } else if nb == last && no == na && nb > 0 && na % nb == 0 {
                        Bcast::LastB
                    } else {
                        return Err(Error::Unsupported("broadcast"));
                    };
                    steps.push(Step::Binary { kind, a: in0, b, out: out0, mode });
                }
                tflite::MEAN => {
                    let axes = ints(*op.inputs.get(1).ok_or(Error::Malformed("mean axes"))?);
                    let [only] = axes.as_slice() else {
                        return Err(Error::Unsupported("mean over several axes"));
                    };
                    if axis(*only, a_shape.len())? + 1 != a_shape.len() {
                        return Err(Error::Unsupported("mean over an inner axis"));
                    }
                    let n = a_shape.last().copied().unwrap_or(1);
                    if sizes[out0] * n != sizes[in0] {
                        return Err(Error::Malformed("mean sizes"));
                    }
                    steps.push(Step::MeanLast { a: in0, out: out0, n });
                }
                tflite::RESHAPE | tflite::SQUEEZE | tflite::EXPAND_DIMS => {
                    let n = sizes[in0];
                    let map = (0..n).map(|i| (0, i as u32)).collect();
                    steps.push(Step::Gather { srcs: vec![in0], map, out: out0 });
                }
                tflite::CONCATENATION => {
                    let ax = axis(op.opts.axis, a_shape.len())?;
                    let outer: usize = a_shape[..ax].iter().product();
                    let mut map = Vec::new();
                    for o in 0..outer {
                        for (k, &src) in ins.iter().enumerate() {
                            let inner: usize =
                                shapes[src].get(ax..).unwrap_or(&[]).iter().product();
                            for i in 0..inner {
                                map.push((k as u32, (o * inner + i) as u32));
                            }
                        }
                    }
                    steps.push(Step::Gather { srcs: ins.clone(), map, out: out0 });
                }
                tflite::PACK => {
                    let ax = axis(op.opts.axis, shapes[out0].len())?;
                    let outer: usize = a_shape.get(..ax).unwrap_or(&[]).iter().product();
                    let inner: usize = a_shape.get(ax..).unwrap_or(&[]).iter().product();
                    let mut map = Vec::new();
                    for o in 0..outer {
                        for k in 0..ins.len() {
                            for i in 0..inner {
                                map.push((k as u32, (o * inner + i) as u32));
                            }
                        }
                    }
                    steps.push(Step::Gather { srcs: ins.clone(), map, out: out0 });
                }
                tflite::UNPACK => {
                    let ax = axis(op.opts.axis, a_shape.len())?;
                    let outer: usize = a_shape[..ax].iter().product();
                    let n = a_shape[ax];
                    let inner: usize = a_shape[ax + 1..].iter().product();
                    for (k, &out) in outs.iter().enumerate() {
                        let mut map = Vec::new();
                        for o in 0..outer {
                            for i in 0..inner {
                                map.push((0, ((o * n + k) * inner + i) as u32));
                            }
                        }
                        steps.push(Step::Gather { srcs: vec![in0], map, out });
                    }
                }
                tflite::SPLIT => {
                    let axis_values = ints(*op.inputs.first().ok_or(Error::Malformed("split"))?);
                    let src = *ins.get(1).ok_or(Error::Malformed("split source"))?;
                    let s = &shapes[src];
                    let ax =
                        axis(*axis_values.first().ok_or(Error::Malformed("split axis"))?, s.len())?;
                    let outer: usize = s[..ax].iter().product();
                    let n = s[ax];
                    let inner: usize = s[ax + 1..].iter().product();
                    let chunk = n / outs.len().max(1);
                    for (k, &out) in outs.iter().enumerate() {
                        let mut map = Vec::new();
                        for o in 0..outer {
                            for j in 0..chunk {
                                for i in 0..inner {
                                    map.push((0, ((o * n + k * chunk + j) * inner + i) as u32));
                                }
                            }
                        }
                        steps.push(Step::Gather { srcs: vec![src], map, out });
                    }
                }
                tflite::STRIDED_SLICE => {
                    let arg = |k: usize| -> Result<Vec<i32>, Error> {
                        Ok(ints(*op.inputs.get(k).ok_or(Error::Malformed("strided slice"))?))
                    };
                    let (begin, end, stride) = (arg(1)?, arg(2)?, arg(3)?);
                    let m = op.opts.masks;
                    if m[2] != 0 || m[3] != 0 {
                        return Err(Error::Unsupported("ellipsis or new-axis strided slice"));
                    }
                    let rank = a_shape.len();
                    if begin.len() < rank || end.len() < rank || stride.len() < rank {
                        return Err(Error::Malformed("strided slice arguments"));
                    }
                    let st = strides(&a_shape);
                    let mut ranges = Vec::new();
                    for d in 0..rank {
                        let dim = a_shape[d] as i32;
                        let norm = |v: i32| if v < 0 { v + dim } else { v };
                        let shrink = (m[4] >> d) & 1 == 1;
                        if stride[d] != 0 && stride[d] != 1 {
                            return Err(Error::Unsupported("strided slice stride"));
                        }
                        let b = if (m[0] >> d) & 1 == 1 && !shrink { 0 } else { norm(begin[d]) };
                        let e = if shrink {
                            b + 1
                        } else if (m[1] >> d) & 1 == 1 {
                            dim
                        } else {
                            norm(end[d]).min(dim)
                        };
                        if b < 0 || e > dim || b >= e {
                            return Err(Error::Malformed("strided slice range"));
                        }
                        ranges.push((b as usize, e as usize));
                    }
                    let mut map = Vec::new();
                    let mut idx: Vec<usize> = ranges.iter().map(|r| r.0).collect();
                    'outer: loop {
                        let flat: usize = idx.iter().zip(&st).map(|(a, b)| a * b).sum();
                        map.push((0, flat as u32));
                        let mut d = rank;
                        loop {
                            if d == 0 {
                                break 'outer;
                            }
                            d -= 1;
                            idx[d] += 1;
                            if idx[d] < ranges[d].1 {
                                break;
                            }
                            idx[d] = ranges[d].0;
                        }
                    }
                    steps.push(Step::Gather { srcs: vec![in0], map, out: out0 });
                }
                _ => return Err(Error::Unsupported("operator")),
            }
        }
        for step in &steps {
            validate(step, &sizes, &consts)?;
        }
        let inputs: Vec<usize> =
            g.inputs.iter().map(|&i| index(i, count)).collect::<Result<_, _>>()?;
        let outputs: Vec<usize> =
            g.outputs.iter().map(|&i| index(i, count)).collect::<Result<_, _>>()?;
        if inputs.iter().chain(&outputs).any(|&t| consts[t].is_some()) {
            return Err(Error::Malformed("constant graph input or output"));
        }
        Ok(Plan { consts, sizes, steps, inputs, outputs, params, macs })
    }

    pub fn new_state(&self) -> PlanState {
        PlanState {
            bufs: self
                .consts
                .iter()
                .zip(&self.sizes)
                .map(|(c, &n)| if c.is_some() { Vec::new() } else { vec![0.0; n] })
                .collect(),
        }
    }

    pub fn run(&self, state: &mut PlanState, kernel: Kernel) {
        let consts = &self.consts;
        for step in &self.steps {
            match step {
                Step::Fc { x, w, b, out, rows, cols } => {
                    let mut o = std::mem::take(&mut state.bufs[*out]);
                    let x = tensor(consts, &state.bufs, *x);
                    let w = tensor(consts, &state.bufs, *w);
                    let bias = b.map(|b| tensor(consts, &state.bufs, b));
                    for (r, (o, row)) in
                        o.iter_mut().zip(w.chunks_exact(*cols)).enumerate().take(*rows)
                    {
                        *o = kernel.dot(row, x) + bias.map_or(0.0, |b| b[r]);
                    }
                    state.bufs[*out] = o;
                }
                Step::Unary { kind, a, out } => {
                    let mut o = std::mem::take(&mut state.bufs[*out]);
                    let a = tensor(consts, &state.bufs, *a);
                    let it = o.iter_mut().zip(a);
                    match kind {
                        Unary::Square => it.for_each(|(o, &x)| *o = x * x),
                        Unary::Log => it.for_each(|(o, &x)| *o = x.ln()),
                        Unary::Rsqrt => it.for_each(|(o, &x)| *o = 1.0 / x.sqrt()),
                        Unary::Sqrt => it.for_each(|(o, &x)| *o = x.sqrt()),
                        Unary::Logistic => it.for_each(|(o, &x)| *o = 1.0 / (1.0 + (-x).exp())),
                        Unary::Tanh => it.for_each(|(o, &x)| *o = x.tanh()),
                    }
                    state.bufs[*out] = o;
                }
                Step::Binary { kind, a, b, out, mode } => {
                    let mut o = std::mem::take(&mut state.bufs[*out]);
                    let a = tensor(consts, &state.bufs, *a);
                    let b = tensor(consts, &state.bufs, *b);
                    let f = |x: f32, y: f32| match kind {
                        Binary::Add => x + y,
                        Binary::Sub => x - y,
                        Binary::Mul => x * y,
                        Binary::Div => x / y,
                        Binary::SquaredDifference => (x - y) * (x - y),
                    };
                    match mode {
                        Bcast::Same => {
                            o.iter_mut().zip(a.iter().zip(b)).for_each(|(o, (&x, &y))| *o = f(x, y))
                        }
                        Bcast::ScalarB => {
                            let y = b.first().copied().unwrap_or(0.0);
                            o.iter_mut().zip(a).for_each(|(o, &x)| *o = f(x, y))
                        }
                        Bcast::ScalarA => {
                            let x = a.first().copied().unwrap_or(0.0);
                            o.iter_mut().zip(b).for_each(|(o, &y)| *o = f(x, y))
                        }
                        Bcast::LastB => {
                            for (o, a) in o.chunks_exact_mut(b.len()).zip(a.chunks_exact(b.len())) {
                                o.iter_mut()
                                    .zip(a.iter().zip(b))
                                    .for_each(|(o, (&x, &y))| *o = f(x, y));
                            }
                        }
                    }
                    state.bufs[*out] = o;
                }
                Step::MeanLast { a, out, n } => {
                    let mut o = std::mem::take(&mut state.bufs[*out]);
                    let a = tensor(consts, &state.bufs, *a);
                    for (v, row) in o.iter_mut().zip(a.chunks_exact(*n)) {
                        *v = row.iter().sum::<f32>() / *n as f32;
                    }
                    state.bufs[*out] = o;
                }
                Step::Gather { srcs, map, out } => {
                    let mut o = std::mem::take(&mut state.bufs[*out]);
                    for (dst, &(k, i)) in o.iter_mut().zip(map.iter()) {
                        *dst = tensor(consts, &state.bufs, srcs[k as usize])[i as usize];
                    }
                    state.bufs[*out] = o;
                }
            }
        }
    }
}

fn tensor<'a>(consts: &'a [Option<Box<[f32]>>], bufs: &'a [Vec<f32>], t: usize) -> &'a [f32] {
    match &consts[t] {
        Some(c) => c,
        None => &bufs[t],
    }
}

/// Every index a step reads is in range, it writes a buffer it does not
/// read, and that buffer is not a constant.
fn validate(step: &Step, sizes: &[usize], consts: &[Option<Box<[f32]>>]) -> Result<(), Error> {
    let (out, reads): (usize, Vec<usize>) = match step {
        Step::Fc { x, w, b, out, .. } => {
            (*out, [Some(*x), Some(*w), *b].into_iter().flatten().collect())
        }
        Step::Unary { a, out, .. } => (*out, vec![*a]),
        Step::Binary { a, b, out, .. } => (*out, vec![*a, *b]),
        Step::MeanLast { a, out, .. } => (*out, vec![*a]),
        Step::Gather { srcs, map, out } => {
            if map.len() != sizes[*out] {
                return Err(Error::Malformed("copy map size"));
            }
            for &(k, i) in map {
                let src = *srcs.get(k as usize).ok_or(Error::Malformed("copy map source"))?;
                if i as usize >= sizes[src] {
                    return Err(Error::Malformed("copy map index"));
                }
            }
            (*out, srcs.clone())
        }
    };
    if consts[out].is_some() || reads.contains(&out) {
        return Err(Error::Malformed("step output"));
    }
    Ok(())
}
