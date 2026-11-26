// Copyright 2025 LiveKit, Inc.
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

use crate::{proto, FfiResult};
use livekit::webrtc::{
    prelude::*,
    video_frame::{native::I420BufferPool, BoxVideoBuffer},
};
use std::slice;

pub mod cvtimpl;

/// The tightly packed pixels of one converted video frame.
///
/// A video stream keeps these and hands the same one back frame after frame,
/// rather than allocating a frame-sized block per frame. That is also why the
/// buffer is reached only through [`fit`](Self::fit): the conversion must size
/// it before writing, and going straight to the bytes would let a caller
/// describe the frame with a length the conversion never produced.
///
/// It is what the FFI handle map stores for a delivered frame, so the handle's
/// type says what it is instead of being an anonymous byte vector.
#[derive(Default)]
pub struct FrameBuffer {
    bytes: Vec<u8>,
}

impl FrameBuffer {
    /// Sizes the buffer to hold exactly `len` bytes and hands back the slice
    /// to write into.
    ///
    /// Capacity is deliberately kept when a later frame is smaller. Reusing
    /// the allocation is the whole point, and a stream whose frame size moves
    /// -- a simulcast layer switch, say -- would otherwise pay a fresh
    /// allocation every time it grew back. This matches `Vec::resize`, which
    /// changes the length and leaves the capacity alone.
    ///
    /// The contents are only zeroed where the buffer grew; the rest is the
    /// previous frame's pixels. Every conversion writes its destination in
    /// full -- destination strides are always tight -- which is what makes
    /// that safe.
    pub(crate) fn fit(&mut self, len: usize) -> &mut [u8] {
        if self.bytes.len() != len {
            self.bytes.clear();
            self.bytes.resize(len, 0);
        }
        self.bytes.as_mut_slice()
    }
}

/// Read-only by design: writes go through [`FrameBuffer::fit`].
impl std::ops::Deref for FrameBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

pub unsafe fn to_libwebrtc_buffer(
    info: proto::VideoBufferInfo,
    pool: &I420BufferPool,
) -> BoxVideoBuffer {
    let r#type = info.r#type();
    let proto::VideoBufferInfo { width, height, components, .. } = info.clone();

    match r#type {
        // For RGBA-family / RGB24 buffers, fused conversion directly into I420Buffer
        proto::VideoBufferType::Rgba => cvtimpl::cvt_rgba_to_i420_buffer(info, false, pool),
        proto::VideoBufferType::Abgr => cvtimpl::cvt_abgr_to_i420_buffer(info, false, pool),
        proto::VideoBufferType::Argb => cvtimpl::cvt_argb_to_i420_buffer(info, false, pool),
        proto::VideoBufferType::Bgra => cvtimpl::cvt_bgra_to_i420_buffer(info, false, pool),
        proto::VideoBufferType::Rgb24 => cvtimpl::cvt_rgb24_to_i420_buffer(info, false, pool),
        proto::VideoBufferType::I420 | proto::VideoBufferType::I420a => {
            let (c0, c1, c2) = (&components[0], &components[1], &components[2]);

            let (data_y, data_u, data_v) = unsafe {
                (
                    slice::from_raw_parts(c0.data_ptr as *const u8, c0.size as usize),
                    slice::from_raw_parts(c1.data_ptr as *const u8, c1.size as usize),
                    slice::from_raw_parts(c2.data_ptr as *const u8, c2.size as usize),
                )
            };
            let chroma_w = (width + 1) / 2;
            let mut i420 = if (c0.stride, c1.stride, c2.stride) == (width, chroma_w, chroma_w) {
                pool.create(width, height)
            } else {
                I420Buffer::with_strides(width, height, c0.stride, c1.stride, c2.stride)
            };

            let (dy, du, dv) = i420.data_mut();
            dy.copy_from_slice(data_y);
            du.copy_from_slice(data_u);
            dv.copy_from_slice(data_v);
            Box::new(i420) as BoxVideoBuffer
        }
        proto::VideoBufferType::I422 => {
            let (c0, c1, c2) = (&components[0], &components[1], &components[2]);

            let (data_y, data_u, data_v) = unsafe {
                (
                    slice::from_raw_parts(c0.data_ptr as *const u8, c0.size as usize),
                    slice::from_raw_parts(c1.data_ptr as *const u8, c1.size as usize),
                    slice::from_raw_parts(c2.data_ptr as *const u8, c2.size as usize),
                )
            };

            let mut i422 = I422Buffer::with_strides(width, height, c0.stride, c1.stride, c2.stride);

            let (dy, du, dv) = i422.data_mut();
            dy.copy_from_slice(data_y);
            du.copy_from_slice(data_u);
            dv.copy_from_slice(data_v);
            Box::new(i422) as BoxVideoBuffer
        }
        proto::VideoBufferType::I444 => {
            let (c0, c1, c2) = (&components[0], &components[1], &components[2]);

            let (data_y, data_u, data_v) = unsafe {
                (
                    slice::from_raw_parts(c0.data_ptr as *const u8, c0.size as usize),
                    slice::from_raw_parts(c1.data_ptr as *const u8, c1.size as usize),
                    slice::from_raw_parts(c2.data_ptr as *const u8, c2.size as usize),
                )
            };
            let mut i444 = I444Buffer::with_strides(width, height, c0.stride, c1.stride, c2.stride);

            let (dy, du, dv) = i444.data_mut();
            dy.copy_from_slice(data_y);
            du.copy_from_slice(data_u);
            dv.copy_from_slice(data_v);
            Box::new(i444) as BoxVideoBuffer
        }
        proto::VideoBufferType::I010 => {
            let (c0, c1, c2) = (&components[0], &components[1], &components[2]);

            let (data_y, data_u, data_v) = unsafe {
                (
                    slice::from_raw_parts(c0.data_ptr as *const u16, c0.size as usize / 2),
                    slice::from_raw_parts(c1.data_ptr as *const u16, c1.size as usize / 2),
                    slice::from_raw_parts(c2.data_ptr as *const u16, c2.size as usize / 2),
                )
            };

            let mut i010 = I010Buffer::with_strides(width, height, c0.stride, c1.stride, c2.stride);

            let (dy, du, dv) = i010.data_mut();
            dy.copy_from_slice(data_y);
            du.copy_from_slice(data_u);
            dv.copy_from_slice(data_v);
            Box::new(i010) as BoxVideoBuffer
        }
        proto::VideoBufferType::Nv12 => {
            let (c0, c1) = (&components[0], &components[1]);

            let (data_y, data_uv) = unsafe {
                (
                    slice::from_raw_parts(c0.data_ptr as *const u8, c0.size as usize),
                    slice::from_raw_parts(c1.data_ptr as *const u8, c1.size as usize),
                )
            };
            let mut nv12 = NV12Buffer::with_strides(info.width, info.height, c0.stride, c1.stride);

            let (dy, duv) = nv12.data_mut();
            dy.copy_from_slice(data_y);
            duv.copy_from_slice(data_uv);
            Box::new(nv12) as BoxVideoBuffer
        }
    }
}

/// Clients may read an I420 frame as one block of width*height + 2*chroma bytes from
/// `data_ptr`, so a decoded buffer can back the handle directly only when it already has the
/// packed layout the copy below produces.
pub fn shared_i420_info(
    rtcbuffer: &dyn VideoBuffer,
    dst_type: Option<proto::VideoBufferType>,
) -> Option<proto::VideoBufferInfo> {
    if !matches!(dst_type, None | Some(proto::VideoBufferType::I420)) {
        return None;
    }
    let i420 = rtcbuffer.as_i420()?;
    let (width, height) = (i420.width(), i420.height());
    let (chroma_width, chroma_height) = ((width + 1) / 2, (height + 1) / 2);
    let (stride_y, stride_u, stride_v) = i420.strides();
    if stride_y != width || stride_u != chroma_width || stride_v != chroma_width {
        return None;
    }
    let (data_y, data_u, data_v) = i420.data();
    let luma_size = (width * height) as usize;
    let chroma_size = (chroma_width * chroma_height) as usize;
    let packed = data_u.as_ptr() as usize == data_y.as_ptr() as usize + luma_size
        && data_v.as_ptr() as usize == data_u.as_ptr() as usize + chroma_size;
    if !packed {
        return None;
    }
    Some(i420_info(
        data_y.as_ptr(),
        data_y.as_ptr(),
        data_u.as_ptr(),
        data_v.as_ptr(),
        width,
        height,
        stride_y,
        stride_u,
        stride_v,
    ))
}

/// Converts a libwebrtc frame buffer into `dst`, tightly packed, and returns
/// the pointers describing it.
///
/// `dst` belongs to the caller so a stream can hand the same allocation back
/// frame after frame instead of churning one frame-sized block per frame. The
/// returned info points into `dst`, so it is only valid while `dst` is.
///
/// A stream skips this copy when the decoder's I420 buffer is already tightly packed: see
/// [`shared_i420_info`], which covers step 1 below.
///
/// TODO(sxian): skip the copy entirely when no conversion was asked for.
///
/// For a native-format request `cvt` only repacks the decoder's padded strides
/// into tight ones. We could hand out the decoder's buffer instead: the handle
/// map can already own one (`impl FfiHandle for BoxVideoBuffer`) and
/// `VideoBufferInfo.components` already carries per-plane stride. Two steps:
///
/// 1. Strides already tight (`stride == width`): the info emitted is identical
///    to what the copy produces, so no binding needs to change.
/// 2. Padded strides: needs stride-aware consumers. The C++ SDK reads
///    `components[].stride` already; Python's `VideoFrame` and Unity's
///    `YuvToRgbConverter` both assume tightly packed data.
///
/// That would also delete the stream's `FrameBuffers`: libwebrtc's
/// `VideoFrameBufferPool` already pools and refcounts decoder buffers, so this
/// layer is only pooling because it copies. Not avoidable when the application
/// requests a different pixel format, or for hardware decode where the source
/// is a platform texture.
pub fn to_video_buffer_info(
    rtcbuffer: BoxVideoBuffer,
    dst_type: Option<proto::VideoBufferType>,
    _normalize_stride: bool, // always normalize stride for now..
    dst: &mut FrameBuffer,
) -> FfiResult<proto::VideoBufferInfo> {
    match rtcbuffer.buffer_type() {
        // Convert Native buffer to I420
        VideoBufferType::Native => {
            let i420 = rtcbuffer.to_i420();
            let (width, height) = (i420.width(), i420.height());
            let (data_y, data_u, data_v) = i420.data();
            let (stride_y, stride_u, stride_v) = i420.strides();
            let info = i420_info(
                data_y.as_ptr(),
                data_y.as_ptr(),
                data_u.as_ptr(),
                data_v.as_ptr(),
                width,
                height,
                stride_y,
                stride_u,
                stride_v,
            );
            // SAFETY: `info` holds raw pointers into `i420`, which `to_i420` returned
            // by value. It lives until the end of this arm, so the conversion has to
            // happen here rather than after the match.
            unsafe {
                cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I420), false, dst)
            }
        }
        VideoBufferType::I420 => {
            let i420 = rtcbuffer.as_i420().unwrap();
            let (width, height) = (i420.width(), i420.height());
            let (data_y, data_u, data_v) = i420.data();
            let (stride_y, stride_u, stride_v) = i420.strides();
            let info = i420_info(
                data_y.as_ptr(),
                data_y.as_ptr(),
                data_u.as_ptr(),
                data_v.as_ptr(),
                width,
                height,
                stride_y,
                stride_u,
                stride_v,
            );
            // SAFETY: `info` holds raw pointers into `i420`, borrowed from
            // `rtcbuffer`, which outlives this call.
            unsafe {
                cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I420), false, dst)
            }
        }
        VideoBufferType::I420A => {
            let i420 = rtcbuffer.as_i420a().unwrap();
            let (width, height) = (i420.width(), i420.height());
            let (stride_y, stride_u, stride_v, stride_a) = i420.strides();
            let (data_y, data_u, data_v, data_a) = i420.data();
            let info = i420a_info(
                data_y.as_ptr(),
                data_y.as_ptr(),
                data_u.as_ptr(),
                data_v.as_ptr(),
                data_a.unwrap().as_ptr(),
                width,
                height,
                stride_y,
                stride_u,
                stride_v,
                stride_a,
            );
            // SAFETY: `info` holds raw pointers into `i420`, borrowed from
            // `rtcbuffer`, which outlives this call.
            unsafe {
                cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I420a), false, dst)
            }
        }
        VideoBufferType::I422 => {
            let i422 = rtcbuffer.as_i422().unwrap();
            let (width, height) = (i422.width(), i422.height());
            let (stride_y, stride_u, stride_v) = i422.strides();
            let (data_y, data_u, data_v) = i422.data();
            let info = i422_info(
                data_y.as_ptr(),
                data_y.as_ptr(),
                data_u.as_ptr(),
                data_v.as_ptr(),
                width,
                height,
                stride_y,
                stride_u,
                stride_v,
            );
            // SAFETY: `info` holds raw pointers into `i422`, borrowed from
            // `rtcbuffer`, which outlives this call.
            unsafe {
                cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I422), false, dst)
            }
        }
        VideoBufferType::I444 => {
            let i444 = rtcbuffer.as_i444().unwrap();
            let (width, height) = (i444.width(), i444.height());
            let (stride_y, stride_u, stride_v) = i444.strides();
            let (data_y, data_u, data_v) = i444.data();
            let info = i444_info(
                data_y.as_ptr(),
                data_y.as_ptr(),
                data_u.as_ptr(),
                data_v.as_ptr(),
                width,
                height,
                stride_y,
                stride_u,
                stride_v,
            );
            // SAFETY: `info` holds raw pointers into `i444`, borrowed from
            // `rtcbuffer`, which outlives this call.
            unsafe {
                cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I444), false, dst)
            }
        }
        VideoBufferType::I010 => {
            let i010 = rtcbuffer.as_i010().unwrap();
            let (width, height) = (i010.width(), i010.height());
            let (stride_y, stride_u, stride_v) = i010.strides();
            let (data_y, data_u, data_v) = i010.data();
            let info = i010_info(
                data_y.as_ptr() as *const u8,
                data_y.as_ptr() as *const u8,
                data_u.as_ptr() as *const u8,
                data_v.as_ptr() as *const u8,
                width,
                height,
                stride_y,
                stride_u,
                stride_v,
            );
            // SAFETY: `info` holds raw pointers into `i010`, borrowed from
            // `rtcbuffer`, which outlives this call.
            unsafe {
                cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I010), false, dst)
            }
        }
        VideoBufferType::NV12 => {
            let nv12 = rtcbuffer.as_nv12().unwrap();
            let (width, height) = (nv12.width(), nv12.height());
            let (stride_y, stride_uv) = nv12.strides();
            let (data_y, data_uv) = nv12.data();
            let info = nv12_info(
                data_y.as_ptr(),
                data_y.as_ptr(),
                data_uv.as_ptr(),
                width,
                height,
                stride_y,
                stride_uv,
            );
            // SAFETY: `info` holds raw pointers into `nv12`, borrowed from
            // `rtcbuffer`, which outlives this call.
            unsafe {
                cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::Nv12), false, dst)
            }
        }
        _ => todo!(),
    }
}

pub fn i420_info(
    data_ptr: *const u8,
    data_y: *const u8,
    data_u: *const u8,
    data_v: *const u8,
    width: u32,
    height: u32,
    stride_y: u32,
    stride_u: u32,
    stride_v: u32,
) -> proto::VideoBufferInfo {
    let chroma_height = (height + 1) / 2;

    let mut components = Vec::with_capacity(3);
    let c1 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_y as u64,
        stride: stride_y,
        size: stride_y * height,
    };

    let c2 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_u as u64,
        stride: stride_u,
        size: stride_u * chroma_height,
    };

    let c3 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_v as u64,
        stride: stride_v,
        size: stride_v * chroma_height,
    };
    components.extend_from_slice(&[c1, c2, c3]);

    proto::VideoBufferInfo {
        width,
        height,
        r#type: proto::VideoBufferType::I420.into(),
        components,
        data_ptr: data_ptr as u64,
        stride: None,
    }
}

pub fn i420a_info(
    data_ptr: *const u8,
    data_y: *const u8,
    data_u: *const u8,
    data_v: *const u8,
    data_a: *const u8,
    width: u32,
    height: u32,
    stride_y: u32,
    stride_u: u32,
    stride_v: u32,
    stride_a: u32,
) -> proto::VideoBufferInfo {
    let chroma_height = (height + 1) / 2;

    let mut components = Vec::with_capacity(4);
    let c1 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_y as u64,
        stride: stride_y,
        size: stride_y * height,
    };

    let c2 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_u as u64,
        stride: stride_u,
        size: stride_u * chroma_height,
    };

    let c3 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_v as u64,
        stride: stride_v,
        size: stride_v * chroma_height,
    };

    let c4 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_a as u64,
        stride: stride_a,
        size: stride_a * height,
    };
    components.extend_from_slice(&[c1, c2, c3, c4]);

    proto::VideoBufferInfo {
        width,
        height,
        r#type: proto::VideoBufferType::I420a.into(),
        components,
        data_ptr: data_ptr as u64,
        stride: None,
    }
}

pub fn i422_info(
    data_ptr: *const u8,
    data_y: *const u8,
    data_u: *const u8,
    data_v: *const u8,
    width: u32,
    height: u32,
    stride_y: u32,
    stride_u: u32,
    stride_v: u32,
) -> proto::VideoBufferInfo {
    let mut components = Vec::with_capacity(3);
    let c1 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_y as u64,
        stride: stride_y,
        size: stride_y * height,
    };

    let c2 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_u as u64,
        stride: stride_u,
        size: stride_u * height,
    };

    let c3 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_v as u64,
        stride: stride_v,
        size: stride_v * height,
    };
    components.extend_from_slice(&[c1, c2, c3]);

    proto::VideoBufferInfo {
        width,
        height,
        r#type: proto::VideoBufferType::I422.into(),
        components,
        data_ptr: data_ptr as u64,
        stride: None,
    }
}

pub fn i444_info(
    data_ptr: *const u8,
    data_y: *const u8,
    data_u: *const u8,
    data_v: *const u8,
    width: u32,
    height: u32,
    stride_y: u32,
    stride_u: u32,
    stride_v: u32,
) -> proto::VideoBufferInfo {
    let mut components = Vec::with_capacity(3);
    let c1 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_y as u64,
        stride: stride_y,
        size: stride_y * height,
    };

    let c2 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_u as u64,
        stride: stride_u,
        size: stride_u * height,
    };

    let c3 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_v as u64,
        stride: stride_v,
        size: stride_v * height,
    };
    components.extend_from_slice(&[c1, c2, c3]);

    proto::VideoBufferInfo {
        width,
        height,
        r#type: proto::VideoBufferType::I444.into(),
        components,
        data_ptr: data_ptr as u64,
        stride: None,
    }
}

pub fn i010_info(
    data_ptr: *const u8,
    data_y: *const u8,
    data_u: *const u8,
    data_v: *const u8,
    width: u32,
    height: u32,
    stride_y: u32,
    stride_u: u32,
    stride_v: u32,
) -> proto::VideoBufferInfo {
    let chroma_height = (height + 1) / 2;

    let mut components = Vec::with_capacity(3);
    let c1 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_y as u64,
        stride: stride_y,
        size: stride_y * height,
    };

    let c2 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_u as u64,
        stride: stride_u,
        size: stride_u * chroma_height,
    };

    let c3 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_v as u64,
        stride: stride_v,
        size: stride_v * chroma_height,
    };
    components.extend_from_slice(&[c1, c2, c3]);

    proto::VideoBufferInfo {
        width,
        height,
        r#type: proto::VideoBufferType::I010.into(),
        components,
        data_ptr: data_ptr as u64,
        stride: None,
    }
}

pub fn nv12_info(
    data_ptr: *const u8,
    data_y: *const u8,
    data_uv: *const u8,
    width: u32,
    height: u32,
    stride_y: u32,
    stride_uv: u32,
) -> proto::VideoBufferInfo {
    let chroma_height = (height + 1) / 2;

    let mut components = Vec::with_capacity(2);
    let c1 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_y as u64,
        stride: stride_y,
        size: stride_y * height,
    };

    let c2 = proto::video_buffer_info::ComponentInfo {
        data_ptr: data_uv as u64,
        stride: stride_uv,
        size: stride_uv * chroma_height * 2,
    };
    components.extend_from_slice(&[c1, c2]);

    proto::VideoBufferInfo {
        width,
        height,
        r#type: proto::VideoBufferType::Nv12.into(),
        components,
        data_ptr: data_ptr as u64,
        stride: None,
    }
}

pub fn rgba_info(
    data_ptr: *const u8,
    r#type: proto::VideoBufferType,
    width: u32,
    height: u32,
) -> proto::VideoBufferInfo {
    proto::VideoBufferInfo {
        width,
        height,
        r#type: r#type.into(),
        components: Vec::default(),
        data_ptr: data_ptr as u64,
        stride: Some(width * 4),
    }
}

pub fn rgb_info(
    data_ptr: *const u8,
    r#type: proto::VideoBufferType,
    width: u32,
    height: u32,
) -> proto::VideoBufferInfo {
    proto::VideoBufferInfo {
        width,
        height,
        r#type: r#type.into(),
        components: Vec::default(),
        data_ptr: data_ptr as u64,
        stride: Some(width * 3),
    }
}

#[cfg(test)]
mod tests {
    use livekit::webrtc::video_frame::I420Buffer;

    use super::*;

    /// A realistic shape: 640x480 I420 is 460,800 bytes.
    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 480;

    fn i420_frame(width: u32, height: u32) -> BoxVideoBuffer {
        Box::new(I420Buffer::new(width, height)) as BoxVideoBuffer
    }

    fn convert_into(dst: &mut FrameBuffer, width: u32, height: u32) -> proto::VideoBufferInfo {
        to_video_buffer_info(
            i420_frame(width, height),
            Some(proto::VideoBufferType::I420),
            true,
            dst,
        )
        .expect("i420 to i420 is supported")
    }

    /// Raised in review: `Vec::resize` does not give capacity back, so a
    /// stream that moves to a smaller frame keeps the larger allocation. That
    /// is deliberate -- reusing it is the point of this type, and a simulcast
    /// layer switch would otherwise pay for the allocation again every time it
    /// moved back up.
    #[test]
    fn a_smaller_frame_keeps_the_allocation_for_when_it_grows_back() {
        let mut buffer = FrameBuffer::default();
        buffer.fit(64 * 1024);
        let large = buffer.as_ptr();

        buffer.fit(1024); // a lower simulcast layer
        buffer.fit(64 * 1024); // and back up again

        assert_eq!(buffer.as_ptr(), large, "the allocation was kept, not freed and remade");
    }

    #[test]
    fn fit_sizes_the_buffer_and_hands_back_what_to_write() {
        let mut buffer = FrameBuffer::default();

        buffer.fit(4).copy_from_slice(&[1, 2, 3, 4]);

        assert_eq!(&buffer[..], &[1, 2, 3, 4]);
        assert_eq!(buffer.fit(2).len(), 2, "a later frame resizes it");
    }

    #[test]
    fn conversion_writes_into_the_buffer_it_is_given() {
        let mut dst = FrameBuffer::default();

        let info = convert_into(&mut dst, WIDTH, HEIGHT);

        let chroma_w = (WIDTH + 1) / 2;
        let chroma_h = (HEIGHT + 1) / 2;
        assert_eq!(dst.len(), (WIDTH * HEIGHT + chroma_w * chroma_h * 2) as usize);
        assert_eq!(info.data_ptr, dst.as_ptr() as u64, "the info points into the caller's buffer");
    }

    #[test]
    fn converting_again_into_the_same_buffer_does_not_reallocate() {
        // This is what lets a stream hand the same allocation back frame after
        // frame instead of churning one per frame.
        let mut dst = FrameBuffer::default();
        convert_into(&mut dst, WIDTH, HEIGHT);
        let address = dst.as_ptr();

        let info = convert_into(&mut dst, WIDTH, HEIGHT);

        assert_eq!(dst.as_ptr(), address, "the second frame reused the allocation");
        assert_eq!(info.data_ptr, address as u64);
    }

    #[test]
    fn a_different_frame_size_resizes_the_buffer() {
        // A simulcast layer switch changes the frame size mid-stream.
        let mut dst = FrameBuffer::default();
        convert_into(&mut dst, WIDTH, HEIGHT);

        let info = convert_into(&mut dst, WIDTH / 2, HEIGHT / 2);

        let chroma_w = (WIDTH / 2 + 1) / 2;
        let chroma_h = (HEIGHT / 2 + 1) / 2;
        assert_eq!(dst.len(), ((WIDTH / 2) * (HEIGHT / 2) + chroma_w * chroma_h * 2) as usize);
        assert_eq!(info.width, WIDTH / 2);
        assert_eq!(info.data_ptr, dst.as_ptr() as u64);
    }

    #[test]
    fn conversion_output_is_tightly_packed() {
        let mut dst = FrameBuffer::default();

        let info = convert_into(&mut dst, WIDTH, HEIGHT);

        let chroma_w = (WIDTH + 1) / 2;
        assert_eq!(info.width, WIDTH);
        assert_eq!(info.height, HEIGHT);
        let [y, u, v] = &info.components[..] else { panic!("expected three planes") };
        assert_eq!((y.stride, u.stride, v.stride), (WIDTH, chroma_w, chroma_w));
    }

    fn filled(mut buffer: I420Buffer) -> I420Buffer {
        let (y, u, v) = buffer.data_mut();
        for (i, b) in y.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        u.fill(64);
        v.fill(192);
        buffer
    }

    fn layout(info: &proto::VideoBufferInfo) -> Vec<(u32, u32)> {
        info.components.iter().map(|c| (c.stride, c.size)).collect()
    }

    #[test]
    fn packed_i420_is_shared_with_the_copy_layout() {
        for (width, height) in [(1280, 720), (641, 359), (2, 2)] {
            let buffer = filled(I420Buffer::new(width, height));
            let (y, u, v) = buffer.data();
            let pointers = [y.as_ptr() as u64, u.as_ptr() as u64, v.as_ptr() as u64];

            let shared = shared_i420_info(&buffer, Some(proto::VideoBufferType::I420)).unwrap();
            assert_eq!(shared_i420_info(&buffer, None).unwrap(), shared);
            assert_eq!(shared.data_ptr, pointers[0]);
            let shared_pointers: Vec<u64> = shared.components.iter().map(|c| c.data_ptr).collect();
            assert_eq!(shared_pointers, pointers);

            let mut copy = FrameBuffer::default();
            let copied = to_video_buffer_info(
                Box::new(filled(I420Buffer::new(width, height))),
                None,
                true,
                &mut copy,
            )
            .unwrap();
            assert_eq!(layout(&shared), layout(&copied));
            assert_eq!(
                (shared.width, shared.height, shared.r#type),
                (width, height, copied.r#type)
            );

            let block = unsafe { slice::from_raw_parts(shared.data_ptr as *const u8, copy.len()) };
            assert_eq!(block, &copy[..]);
        }
    }

    #[test]
    fn padded_or_converted_i420_is_not_shared() {
        let padded = I420Buffer::with_strides(640, 360, 704, 352, 352);
        assert!(shared_i420_info(&padded, None).is_none());

        let packed = I420Buffer::new(640, 360);
        assert!(shared_i420_info(&packed, Some(proto::VideoBufferType::Rgba)).is_none());
        assert!(shared_i420_info(&packed, Some(proto::VideoBufferType::Nv12)).is_none());
    }

    fn noise(len: usize, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2654435761) | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 8) as u8
            })
            .collect()
    }

    fn planes(buffer: &I420Buffer) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (y, u, v) = buffer.data();
        (y.to_vec(), u.to_vec(), v.to_vec())
    }

    fn unpooled_i420(
        r#type: proto::VideoBufferType,
        src: &[u8],
        width: u32,
        height: u32,
    ) -> I420Buffer {
        use imgproc::colorcvt;
        let chroma_w = (width + 1) / 2;
        let mut i420 = I420Buffer::with_strides(width, height, width, chroma_w, chroma_w);
        let (y, u, v) = i420.data_mut();
        y.fill(0x55);
        u.fill(0x55);
        v.fill(0x55);
        let convert = match r#type {
            proto::VideoBufferType::Rgba => colorcvt::abgr_to_i420,
            proto::VideoBufferType::Abgr => colorcvt::rgba_to_i420,
            proto::VideoBufferType::Argb => colorcvt::bgra_to_i420,
            proto::VideoBufferType::Bgra => colorcvt::argb_to_i420,
            proto::VideoBufferType::Rgb24 => colorcvt::raw_to_i420,
            _ => unreachable!(),
        };
        let bpp = if r#type == proto::VideoBufferType::Rgb24 { 3 } else { 4 };
        convert(src, width * bpp, y, width, u, chroma_w, v, chroma_w, width, height, false);
        i420
    }

    fn dirty_pool_buffer(pool: &I420BufferPool, width: u32, height: u32) -> *const u8 {
        let mut buffer = pool.create(width, height);
        let (y, u, v) = buffer.data_mut();
        y.fill(0xAA);
        u.fill(0xAA);
        v.fill(0xAA);
        y.as_ptr()
    }

    #[test]
    fn pooled_capture_conversions_match_the_unpooled_path() {
        let types = [
            proto::VideoBufferType::Rgba,
            proto::VideoBufferType::Abgr,
            proto::VideoBufferType::Argb,
            proto::VideoBufferType::Bgra,
            proto::VideoBufferType::Rgb24,
        ];
        let pool = I420BufferPool::new(2);
        for (width, height) in [(641, 359), (1280, 720), (3, 3), (1, 1)] {
            for (i, r#type) in types.into_iter().enumerate() {
                let bpp = if r#type == proto::VideoBufferType::Rgb24 { 3 } else { 4 };
                let src = noise((width * height * bpp) as usize, i as u32 + width);
                let info = proto::VideoBufferInfo {
                    r#type: r#type.into(),
                    width,
                    height,
                    data_ptr: src.as_ptr() as u64,
                    stride: Some(width * bpp),
                    components: Vec::new(),
                };

                let reused = dirty_pool_buffer(&pool, width, height);
                let pooled = unsafe { to_libwebrtc_buffer(info, &pool) };
                let pooled = pooled.as_i420().unwrap();
                assert_eq!(pooled.data().0.as_ptr(), reused, "{:?} {width}x{height}", r#type);

                let unpooled = unpooled_i420(r#type, &src, width, height);
                assert_eq!(pooled.strides(), unpooled.strides());
                assert!(planes(pooled) == planes(&unpooled), "{:?} {width}x{height}", r#type);
            }
        }
    }

    #[test]
    fn packed_i420_capture_uses_the_pool_and_padded_keeps_its_strides() {
        let pool = I420BufferPool::new(2);
        for (width, height) in [(641, 359), (640, 360)] {
            let chroma_w = (width + 1) / 2;
            let chroma_size = (chroma_w * ((height + 1) / 2)) as usize;
            let y = noise((width * height) as usize, 1);
            let u = noise(chroma_size, 2);
            let v = noise(chroma_size, 3);
            let info = i420_info(
                y.as_ptr(),
                y.as_ptr(),
                u.as_ptr(),
                v.as_ptr(),
                width,
                height,
                width,
                chroma_w,
                chroma_w,
            );

            let reused = dirty_pool_buffer(&pool, width, height);
            let pooled = unsafe { to_libwebrtc_buffer(info, &pool) };
            let pooled = pooled.as_i420().unwrap();
            assert_eq!(pooled.data().0.as_ptr(), reused);
            assert!(planes(pooled) == (y, u, v));
        }

        let padded = filled(I420Buffer::with_strides(640, 360, 704, 352, 352));
        let (y, u, v) = padded.data();
        let info =
            i420_info(y.as_ptr(), y.as_ptr(), u.as_ptr(), v.as_ptr(), 640, 360, 704, 352, 352);
        let copied = unsafe { to_libwebrtc_buffer(info, &pool) };
        let copied = copied.as_i420().unwrap();
        assert_eq!(copied.strides(), (704, 352, 352));
        assert!(planes(copied) == planes(&padded));
    }

    #[test]
    fn padded_i420_copy_is_packed() {
        let padded = filled(I420Buffer::with_strides(640, 360, 704, 352, 352));
        let mut dst = FrameBuffer::default();
        let info = to_video_buffer_info(Box::new(padded), None, true, &mut dst).unwrap();
        assert_eq!(layout(&info), vec![(640, 640 * 360), (320, 320 * 180), (320, 320 * 180)]);
    }
}
