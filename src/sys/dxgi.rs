//! Minimal DXGI adapter enumeration, used to map a GPU's LUID to its real name
//! and dedicated video memory.
//!
//! The PDH `GPU Engine` counters identify adapters only by LUID, so without this
//! the Performance tab can label graphs "GPU 0" but cannot say *which* GPU, nor
//! what fraction of its VRAM is in use.
//!
//! `windows-sys` exposes no COM interfaces (it is the raw bindings crate), so
//! the two vtable slots we need are called directly. Both indices are part of
//! DXGI's frozen ABI and have not moved since Windows 7. Everything is
//! validated and the whole module degrades to `None`.

use std::ffi::c_void;
use windows_sys::core::GUID;

#[link(name = "dxgi")]
extern "system" {
    fn CreateDXGIFactory1(riid: *const GUID, ppfactory: *mut *mut c_void) -> i32;
}

// IID_IDXGIFactory1 {770aae78-f26f-4dba-a829-253c83d1b387}
const IID_IDXGIFACTORY1: GUID = GUID {
    data1: 0x770a_ae78,
    data2: 0xf26f,
    data3: 0x4dba,
    data4: [0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87],
};

/// `IUnknown::Release`
const SLOT_RELEASE: usize = 2;
/// `IDXGIFactory1::EnumAdapters1` -- IUnknown(3) + IDXGIObject(4) + IDXGIFactory(5)
const SLOT_ENUM_ADAPTERS1: usize = 12;
/// `IDXGIAdapter1::GetDesc1` -- IUnknown(3) + IDXGIObject(4) + IDXGIAdapter(3)
const SLOT_GET_DESC1: usize = 10;

const DXGI_ERROR_NOT_FOUND: i32 = 0x887A_0002u32 as i32;
/// `DXGI_ADAPTER_FLAG_SOFTWARE`
const ADAPTER_FLAG_SOFTWARE: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct AdapterDesc1 {
    description: [u16; 128],       // 0x000
    vendor_id: u32,                // 0x100
    device_id: u32,                // 0x104
    sub_sys_id: u32,               // 0x108
    revision: u32,                 // 0x10C
    dedicated_video_memory: usize, // 0x110
    dedicated_system_memory: usize,// 0x118
    shared_system_memory: usize,   // 0x120
    luid_low: u32,                 // 0x128
    luid_high: i32,                // 0x12C
    flags: u32,                    // 0x130
}

const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(offset_of!(AdapterDesc1, vendor_id) == 0x100);
    assert!(offset_of!(AdapterDesc1, dedicated_video_memory) == 0x110);
    assert!(offset_of!(AdapterDesc1, luid_low) == 0x128);
    assert!(offset_of!(AdapterDesc1, flags) == 0x130);
    assert!(size_of::<AdapterDesc1>() == 0x138);
};

/// What we learned about one adapter.
pub struct Adapter {
    /// `(high as u64) << 32 | low`, matching the key built from PDH instances.
    pub luid: u64,
    pub name: String,
    pub dedicated_video_memory: u64,
    pub is_software: bool,
}

#[inline]
unsafe fn vtable_slot(obj: *mut c_void, index: usize) -> *const () {
    // A COM object begins with a pointer to its vtable, an array of fn pointers.
    let vtbl = *(obj as *const *const *const ());
    *vtbl.add(index)
}

#[inline]
unsafe fn release(obj: *mut c_void) {
    type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
    let f: ReleaseFn = std::mem::transmute(vtable_slot(obj, SLOT_RELEASE));
    f(obj);
}

/// Enumerate the machine's graphics adapters. Returns an empty vec if DXGI is
/// unavailable, which is normal in some server and container images.
pub fn enumerate() -> Vec<Adapter> {
    type EnumAdapters1Fn =
        unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
    type GetDesc1Fn = unsafe extern "system" fn(*mut c_void, *mut AdapterDesc1) -> i32;

    let mut out = Vec::new();

    let mut factory: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { CreateDXGIFactory1(&IID_IDXGIFACTORY1, &mut factory) };
    if hr != 0 || factory.is_null() {
        return out;
    }

    unsafe {
        let enum_adapters: EnumAdapters1Fn =
            std::mem::transmute(vtable_slot(factory, SLOT_ENUM_ADAPTERS1));

        for index in 0..64u32 {
            let mut adapter: *mut c_void = std::ptr::null_mut();
            let hr = enum_adapters(factory, index, &mut adapter);
            if hr == DXGI_ERROR_NOT_FOUND || hr != 0 || adapter.is_null() {
                break;
            }

            let get_desc: GetDesc1Fn = std::mem::transmute(vtable_slot(adapter, SLOT_GET_DESC1));
            let mut desc: AdapterDesc1 = std::mem::zeroed();
            let hr = get_desc(adapter, &mut desc);
            release(adapter);

            if hr != 0 {
                continue;
            }

            let name = super::util::from_wide_nul(&desc.description);
            let luid = ((desc.luid_high as u32 as u64) << 32) | desc.luid_low as u64;

            // A zero LUID or an empty name means we are not reading what we
            // think we are; abandon the whole attempt rather than show garbage.
            if luid == 0 || name.is_empty() {
                out.clear();
                break;
            }

            out.push(Adapter {
                luid,
                name,
                dedicated_video_memory: desc.dedicated_video_memory as u64,
                is_software: desc.flags & ADAPTER_FLAG_SOFTWARE != 0,
            });
        }

        release(factory);
    }

    out
}
