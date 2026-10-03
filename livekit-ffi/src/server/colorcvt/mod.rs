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
use livekit::webrtc::{prelude::*, video_frame::BoxVideoBuffer};
use std::slice;

pub mod cvtimpl;

pub unsafe fn to_libwebrtc_buffer(info: proto::VideoBufferInfo) -> BoxVideoBuffer {
    let r#type = info.r#type();
    let proto::VideoBufferInfo { width, height, components, .. } = info.clone();

    match r#type {
        // For RGBA-family / RGB24 buffers, fused conversion directly into I420Buffer
        proto::VideoBufferType::Rgba => cvtimpl::cvt_rgba_to_i420_buffer(info, false),
        proto::VideoBufferType::Abgr => cvtimpl::cvt_abgr_to_i420_buffer(info, false),
        proto::VideoBufferType::Argb => cvtimpl::cvt_argb_to_i420_buffer(info, false),
        proto::VideoBufferType::Bgra => cvtimpl::cvt_bgra_to_i420_buffer(info, false),
        proto::VideoBufferType::Rgb24 => cvtimpl::cvt_rgb24_to_i420_buffer(info, false),
        proto::VideoBufferType::I420 | proto::VideoBufferType::I420a => {
            let (c0, c1, c2) = (&components[0], &components[1], &components[2]);

            let (data_y, data_u, data_v) = unsafe {
                (
                    slice::from_raw_parts(c0.data_ptr as *const u8, c0.size as usize),
                    slice::from_raw_parts(c1.data_ptr as *const u8, c1.size as usize),
                    slice::from_raw_parts(c2.data_ptr as *const u8, c2.size as usize),
                )
            };
            let mut i420 = I420Buffer::with_strides(width, height, c0.stride, c1.stride, c2.stride);

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

pub fn to_video_buffer_info(
    rtcbuffer: BoxVideoBuffer,
    dst_type: Option<proto::VideoBufferType>,
    _normalize_stride: bool, // always normalize stride for now..
) -> FfiResult<(Box<[u8]>, proto::VideoBufferInfo)> {
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
            unsafe { cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I420), false) }
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
            unsafe { cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I420), false) }
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
            unsafe { cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I420a), false) }
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
            unsafe { cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I422), false) }
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
            unsafe { cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I444), false) }
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
            unsafe { cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::I010), false) }
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
            unsafe { cvtimpl::cvt(info, dst_type.unwrap_or(proto::VideoBufferType::Nv12), false) }
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
    use super::*;

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

            let (copy, copied) =
                to_video_buffer_info(Box::new(filled(I420Buffer::new(width, height))), None, true)
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

    #[test]
    fn padded_i420_copy_is_packed() {
        let padded = filled(I420Buffer::with_strides(640, 360, 704, 352, 352));
        let (_, info) = to_video_buffer_info(Box::new(padded), None, true).unwrap();
        assert_eq!(layout(&info), vec![(640, 640 * 360), (320, 320 * 180), (320, 320 * 180)]);
    }
}
