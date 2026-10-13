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

//! The part of the TFLite flatbuffer schema (v3) a float graph of DTLN-aec
//! needs. Every read is bounds-checked: a malformed file is an error, never a
//! panic.

use crate::Error;

#[derive(Clone, Copy)]
struct Fb<'a> {
    b: &'a [u8],
}

impl<'a> Fb<'a> {
    fn bytes<const N: usize>(&self, p: usize) -> Option<[u8; N]> {
        self.b.get(p..p.checked_add(N)?)?.try_into().ok()
    }
    fn u8(&self, p: usize) -> Option<u8> {
        self.b.get(p).copied()
    }
    fn u16(&self, p: usize) -> Option<u16> {
        self.bytes::<2>(p).map(u16::from_le_bytes)
    }
    fn u32(&self, p: usize) -> Option<u32> {
        self.bytes::<4>(p).map(u32::from_le_bytes)
    }
    fn i32(&self, p: usize) -> Option<i32> {
        self.bytes::<4>(p).map(i32::from_le_bytes)
    }
    fn u64(&self, p: usize) -> Option<u64> {
        self.bytes::<8>(p).map(u64::from_le_bytes)
    }
    fn field(&self, table: usize, id: usize) -> Option<usize> {
        let vt = usize::try_from(i64::try_from(table).ok()? - i64::from(self.i32(table)?)).ok()?;
        let vt_size = usize::from(self.u16(vt)?);
        let slot = 4 + 2 * id;
        if slot + 2 > vt_size {
            return None;
        }
        let off = usize::from(self.u16(vt + slot)?);
        if off == 0 {
            None
        } else {
            table.checked_add(off)
        }
    }
    fn deref(&self, p: usize) -> Option<usize> {
        p.checked_add(usize::try_from(self.u32(p)?).ok()?)
    }
    fn table(&self, table: usize, id: usize) -> Option<usize> {
        self.deref(self.field(table, id)?)
    }
    fn vector(&self, table: usize, id: usize) -> Option<(usize, usize)> {
        let v = self.deref(self.field(table, id)?)?;
        Some((v.checked_add(4)?, usize::try_from(self.u32(v)?).ok()?))
    }
    fn ints(&self, table: usize, id: usize) -> Result<Vec<i32>, Error> {
        match self.vector(table, id) {
            Some((s, n)) => {
                (0..n).map(|i| self.i32(s + 4 * i).ok_or(Error::Malformed("int vector"))).collect()
            }
            None => Ok(Vec::new()),
        }
    }
    fn tables(&self, table: usize, id: usize) -> Result<Vec<usize>, Error> {
        match self.vector(table, id) {
            Some((s, n)) => (0..n)
                .map(|i| self.deref(s + 4 * i).ok_or(Error::Malformed("table vector")))
                .collect(),
            None => Ok(Vec::new()),
        }
    }
    fn u8_or(&self, table: usize, id: usize, d: u8) -> u8 {
        self.field(table, id).and_then(|p| self.u8(p)).unwrap_or(d)
    }
    fn i32_or(&self, table: usize, id: usize, d: i32) -> i32 {
        self.field(table, id).and_then(|p| self.i32(p)).unwrap_or(d)
    }
}

pub(crate) struct Tensor {
    pub shape: Vec<i32>,
    pub ty: u8,
    pub data: Option<Vec<u8>>,
}

#[derive(Default)]
pub(crate) struct Options {
    pub activation: u8,
    pub axis: i32,
    pub masks: [i32; 5],
}

pub(crate) struct Op {
    pub code: i32,
    pub inputs: Vec<i32>,
    pub outputs: Vec<i32>,
    pub opts: Options,
}

pub(crate) struct Graph {
    pub tensors: Vec<Tensor>,
    pub inputs: Vec<i32>,
    pub outputs: Vec<i32>,
    pub ops: Vec<Op>,
}

pub(crate) const ADD: i32 = 0;
pub(crate) const CONCATENATION: i32 = 2;
pub(crate) const CONV_2D: i32 = 3;
pub(crate) const FULLY_CONNECTED: i32 = 9;
pub(crate) const LOGISTIC: i32 = 14;
pub(crate) const MUL: i32 = 18;
pub(crate) const RESHAPE: i32 = 22;
pub(crate) const TANH: i32 = 28;
pub(crate) const MEAN: i32 = 40;
pub(crate) const SUB: i32 = 41;
pub(crate) const DIV: i32 = 42;
pub(crate) const SQUEEZE: i32 = 43;
pub(crate) const STRIDED_SLICE: i32 = 45;
pub(crate) const SPLIT: i32 = 49;
pub(crate) const EXPAND_DIMS: i32 = 70;
pub(crate) const LOG: i32 = 73;
pub(crate) const SQRT: i32 = 75;
pub(crate) const RSQRT: i32 = 76;
pub(crate) const PACK: i32 = 83;
pub(crate) const UNPACK: i32 = 88;
pub(crate) const SQUARE: i32 = 92;
pub(crate) const SQUARED_DIFFERENCE: i32 = 99;

pub(crate) const FLOAT32: u8 = 0;
const INT32: u8 = 2;
const INT64: u8 = 4;

fn options(fb: Fb, op: usize, code: i32) -> Options {
    let mut o = Options::default();
    let Some(t) = fb.table(op, 4) else { return o };
    match code {
        ADD | MUL | SUB | DIV | FULLY_CONNECTED => o.activation = fb.u8_or(t, 0, 0),
        CONCATENATION => {
            o.axis = fb.i32_or(t, 0, 0);
            o.activation = fb.u8_or(t, 1, 0);
        }
        STRIDED_SLICE => {
            for (i, mask) in o.masks.iter_mut().enumerate() {
                *mask = fb.i32_or(t, i, 0);
            }
        }
        PACK | UNPACK => o.axis = fb.i32_or(t, 1, 0),
        CONV_2D => o.activation = fb.u8_or(t, 3, 0),
        _ => {}
    }
    o
}

pub(crate) fn load(bytes: &[u8]) -> Result<Graph, Error> {
    let fb = Fb { b: bytes };
    let root = fb.deref(0).ok_or(Error::Malformed("root"))?;
    let codes: Vec<i32> = fb
        .tables(root, 1)?
        .into_iter()
        .map(|c| {
            let deprecated =
                fb.field(c, 0).and_then(|p| fb.u8(p)).map_or(0, |v| i32::from(v as i8));
            deprecated.max(fb.i32_or(c, 3, 0))
        })
        .collect();
    let mut buffers: Vec<Option<Vec<u8>>> = Vec::new();
    for b in fb.tables(root, 4)? {
        if let Some((s, n)) = fb.vector(b, 0) {
            if n > 0 {
                let data = bytes.get(s..s.checked_add(n).ok_or(Error::Malformed("buffer"))?);
                buffers.push(Some(data.ok_or(Error::Malformed("buffer"))?.to_vec()));
                continue;
            }
        }
        let off = fb.field(b, 1).and_then(|p| fb.u64(p)).unwrap_or(0);
        let size = fb.field(b, 2).and_then(|p| fb.u64(p)).unwrap_or(0);
        if off > 1 && size > 0 {
            let off = usize::try_from(off).map_err(|_| Error::Malformed("buffer offset"))?;
            let size = usize::try_from(size).map_err(|_| Error::Malformed("buffer size"))?;
            let data = bytes.get(off..off.checked_add(size).ok_or(Error::Malformed("buffer"))?);
            buffers.push(Some(data.ok_or(Error::Malformed("buffer"))?.to_vec()));
        } else {
            buffers.push(None);
        }
    }
    let subgraphs = fb.tables(root, 2)?;
    let [sg] = subgraphs.as_slice() else {
        return Err(Error::Malformed("not one subgraph"));
    };
    let sg = *sg;
    let mut tensors = Vec::new();
    for t in fb.tables(sg, 0)? {
        let buffer = fb.field(t, 2).and_then(|p| fb.u32(p)).unwrap_or(0) as usize;
        tensors.push(Tensor {
            shape: fb.ints(t, 0)?,
            ty: fb.u8_or(t, 1, 0),
            data: buffers.get(buffer).cloned().flatten(),
        });
    }
    let mut ops = Vec::new();
    for op in fb.tables(sg, 3)? {
        let index = fb.field(op, 0).and_then(|p| fb.u32(p)).unwrap_or(0) as usize;
        let code = *codes.get(index).ok_or(Error::Malformed("opcode index"))?;
        ops.push(Op {
            code,
            inputs: fb.ints(op, 1)?,
            outputs: fb.ints(op, 2)?,
            opts: options(fb, op, code),
        });
    }
    Ok(Graph { tensors, inputs: fb.ints(sg, 1)?, outputs: fb.ints(sg, 2)?, ops })
}

impl Tensor {
    pub fn floats(&self) -> Option<Vec<f32>> {
        let d = self.data.as_ref()?;
        match self.ty {
            FLOAT32 => Some(
                d.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
            ),
            _ => None,
        }
    }

    pub fn ints(&self) -> Option<Vec<i32>> {
        let d = self.data.as_ref()?;
        match self.ty {
            INT32 => Some(
                d.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
            ),
            INT64 => Some(
                d.chunks_exact(8)
                    .map(|c| {
                        let v =
                            i64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                        i32::try_from(v).unwrap_or(i32::MAX)
                    })
                    .collect(),
            ),
            _ => None,
        }
    }
}
