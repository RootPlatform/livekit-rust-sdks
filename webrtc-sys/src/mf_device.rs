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

//! The MediaFoundation codecs' shared D3D11 device, handed to the host (see
//! `AcquireSharedD3D11DeviceForHost` in `src/mf/mf_common.h`).

use std::ffi::c_void;

/// A DXGI adapter LUID, laid out like the Win32 `LUID`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Luid {
    pub low_part: u32,
    pub high_part: i32,
}

extern "C" {
    /// `adapter_luid` may be null for the default adapter. Returns an HRESULT;
    /// on success `*out_device` is an `ID3D11Device*` holding one COM reference.
    pub fn lk_mf_acquire_host_d3d11_device(
        adapter_luid: *const Luid,
        out_device: *mut *mut c_void,
    ) -> i32;

    /// Returns an HRESULT: E_INVALIDARG when `device` holds no host reference.
    pub fn lk_mf_release_host_d3d11_device(device: *mut c_void) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    const S_OK: i32 = 0;
    const E_POINTER: i32 = 0x8000_4003_u32 as i32;
    const E_INVALIDARG: i32 = 0x8007_0057_u32 as i32;
    const DXGI_ERROR_DEVICE_REMOVED: i32 = 0x887A_0005_u32 as i32;
    const D3D11_CREATE_DEVICE_BGRA_SUPPORT: u32 = 0x20;
    const D3D11_CREATE_DEVICE_VIDEO_SUPPORT: u32 = 0x800;

    unsafe fn vtable_fn(object: *mut c_void, slot: usize) -> *const c_void {
        let vtable = *(object as *const *const *const c_void);
        *vtable.add(slot)
    }

    unsafe fn add_ref(object: *mut c_void) -> u32 {
        let f: extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(vtable_fn(object, 1));
        f(object)
    }

    unsafe fn release(object: *mut c_void) -> u32 {
        let f: extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(vtable_fn(object, 2));
        f(object)
    }

    unsafe fn ref_count(object: *mut c_void) -> u32 {
        add_ref(object);
        release(object)
    }

    unsafe fn creation_flags(device: *mut c_void) -> u32 {
        let f: extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(vtable_fn(device, 38));
        f(device)
    }

    unsafe fn removed_reason(device: *mut c_void) -> i32 {
        let f: extern "system" fn(*mut c_void) -> i32 = std::mem::transmute(vtable_fn(device, 39));
        f(device)
    }

    unsafe fn acquire(luid: Option<&Luid>) -> (i32, *mut c_void) {
        let mut device = std::ptr::null_mut();
        let hr = lk_mf_acquire_host_d3d11_device(
            luid.map_or(std::ptr::null(), |l| l as *const Luid),
            &mut device,
        );
        (hr, device)
    }

    // One test, so the fault switch it sets never races another acquire.
    #[test]
    fn host_device_contract() {
        unsafe {
            assert_eq!(
                lk_mf_acquire_host_d3d11_device(std::ptr::null(), std::ptr::null_mut()),
                E_POINTER
            );
            assert_eq!(lk_mf_release_host_d3d11_device(std::ptr::null_mut()), E_POINTER);

            let (hr, first) = acquire(None);
            if hr != S_OK {
                eprintln!("no hardware D3D11 video device here ({hr:#x}); skipping");
                assert!(first.is_null());
                return;
            }
            assert!(!first.is_null());
            let flags = creation_flags(first);
            assert_eq!(
                flags & D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT
            );
            assert_eq!(flags & D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_BGRA_SUPPORT);

            let (hr, second) = acquire(None);
            assert_eq!(hr, S_OK);
            assert_eq!(second, first, "one shared device per adapter");
            let held = ref_count(first);

            // The host references keep the device alive after the caller's COM
            // references are gone.
            release(second);
            release(first);
            assert_eq!(ref_count(first), held - 2);
            assert_eq!(removed_reason(first), S_OK);

            let bogus = Luid { low_part: 0xFFFF_FFF0, high_part: 0x7FFF_FFF0 };
            let (hr, none) = acquire(Some(&bogus));
            assert!(hr < 0);
            assert!(none.is_null());

            std::env::set_var("LK_MF_FAULT_HOST_DEVICE_REMOVED", "1");
            let (hr, removed) = acquire(None);
            std::env::remove_var("LK_MF_FAULT_HOST_DEVICE_REMOVED");
            assert_eq!(hr, DXGI_ERROR_DEVICE_REMOVED);
            assert!(removed.is_null());

            assert_eq!(lk_mf_release_host_d3d11_device(first), S_OK);
            assert_eq!(lk_mf_release_host_d3d11_device(first), S_OK);
            assert_eq!(lk_mf_release_host_d3d11_device(first), E_INVALIDARG);
        }
    }
}
