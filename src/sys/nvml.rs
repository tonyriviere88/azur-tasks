//! NVIDIA sensor readings through NVML, loaded at run time.
//!
//! NVML (`nvml.dll`, shipped with the display driver) is the only interface on
//! Windows that reports a GPU's temperature, fan and board power to an
//! unprivileged process. There is no vendor-neutral equivalent: D3D and the
//! performance counters know about *load* and *memory*, never about heat.
//!
//! Bound by name at run time rather than linked, because the DLL is present only
//! on machines with an NVIDIA driver -- linking it would refuse to start the app
//! everywhere else. A machine without it simply has no NVIDIA sensors, and the
//! tab says so instead of showing zeros.

use super::snapshot::{vendor, GpuSensors};
use super::util::{dll_symbol as symbol, load_dll};
use std::ffi::c_void;
use std::sync::Arc;

type Handle = *mut c_void;

// NVML's ABI. Every entry point returns `nvmlReturn_t`, 0 on success.
type FnInit = unsafe extern "system" fn() -> i32;
type FnShutdown = unsafe extern "system" fn() -> i32;
type FnCount = unsafe extern "system" fn(*mut u32) -> i32;
type FnByIndex = unsafe extern "system" fn(u32, *mut Handle) -> i32;
type FnName = unsafe extern "system" fn(Handle, *mut u8, u32) -> i32;
type FnTemp = unsafe extern "system" fn(Handle, u32, *mut u32) -> i32;
type FnFan = unsafe extern "system" fn(Handle, *mut u32) -> i32;
type FnClock = unsafe extern "system" fn(Handle, u32, *mut u32) -> i32;
type FnPower = unsafe extern "system" fn(Handle, *mut u32) -> i32;
type FnUtil = unsafe extern "system" fn(Handle, *mut Utilization) -> i32;
type FnMemory = unsafe extern "system" fn(Handle, *mut Memory) -> i32;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: u32,
    memory: u32,
}

/// `nvmlMemory_t`: the v1 layout, which every driver still accepts.
#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

const TEMPERATURE_GPU: u32 = 0;
const CLOCK_GRAPHICS: u32 = 0;
const CLOCK_MEM: u32 = 2;

pub struct Nvml {
    shutdown: FnShutdown,
    by_index: FnByIndex,
    name: FnName,
    temp: FnTemp,
    fan: Option<FnFan>,
    clock: Option<FnClock>,
    power: Option<FnPower>,
    util: Option<FnUtil>,
    memory: Option<FnMemory>,
    count: u32,
}

impl Nvml {
    /// Load NVML and initialize it. `None` on any machine without an NVIDIA
    /// driver, and on one whose driver is too old for the symbols used here.
    pub fn load() -> Option<Self> {
        let module = load_dll("nvml.dll")?;

        // The `_v2` entry points date from 2012; asking for them by name keeps
        // the ABI pinned, which the unversioned aliases do not promise.
        let init: FnInit = unsafe { symbol(module, b"nvmlInit_v2\0")? };
        let shutdown: FnShutdown = unsafe { symbol(module, b"nvmlShutdown\0")? };
        let count_fn: FnCount = unsafe { symbol(module, b"nvmlDeviceGetCount_v2\0")? };
        let by_index: FnByIndex = unsafe { symbol(module, b"nvmlDeviceGetHandleByIndex_v2\0")? };
        let name: FnName = unsafe { symbol(module, b"nvmlDeviceGetName\0")? };
        let temp: FnTemp = unsafe { symbol(module, b"nvmlDeviceGetTemperature\0")? };

        if unsafe { init() } != 0 {
            return None;
        }
        let mut count = 0u32;
        if unsafe { count_fn(&mut count) } != 0 || count == 0 {
            unsafe { shutdown() };
            return None;
        }

        Some(Self {
            shutdown,
            by_index,
            name,
            temp,
            // The rest are optional: a driver that lacks one still gives the
            // others, and one missing reading should not cost all of them.
            fan: unsafe { symbol(module, b"nvmlDeviceGetFanSpeed\0") },
            clock: unsafe { symbol(module, b"nvmlDeviceGetClockInfo\0") },
            power: unsafe { symbol(module, b"nvmlDeviceGetPowerUsage\0") },
            util: unsafe { symbol(module, b"nvmlDeviceGetUtilizationRates\0") },
            memory: unsafe { symbol(module, b"nvmlDeviceGetMemoryInfo\0") },
            count,
        })
    }

    pub fn device_count(&self) -> u32 {
        self.count
    }

    /// Read every sensor of device `i`, skipping the ones it does not have.
    pub fn read(&self, i: u32) -> Option<GpuSensors> {
        let mut handle: Handle = std::ptr::null_mut();
        if unsafe { (self.by_index)(i, &mut handle) } != 0 || handle.is_null() {
            return None;
        }

        let mut buf = [0u8; 96];
        let name = if unsafe { (self.name)(handle, buf.as_mut_ptr(), buf.len() as u32) } == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[..end]).into_owned()
        } else {
            format!("NVIDIA device {i}")
        };

        let mut dev = GpuSensors {
            vendor: vendor::NVIDIA,
            ordinal: i,
            source: "NVML",
            name: Arc::from(name.as_str()),
            ..Default::default()
        };

        let mut v = 0u32;
        if unsafe { (self.temp)(handle, TEMPERATURE_GPU, &mut v) } == 0 {
            dev.set_temp(v as f32);
        }
        if let Some(f) = self.fan {
            let mut pct = 0u32;
            if unsafe { f(handle, &mut pct) } == 0 {
                dev.set_fan_pct(pct as f32);
            }
        }
        if let Some(f) = self.clock {
            let mut mhz = 0u32;
            if unsafe { f(handle, CLOCK_GRAPHICS, &mut mhz) } == 0 {
                dev.set_core_mhz(mhz as f64);
            }
            if unsafe { f(handle, CLOCK_MEM, &mut mhz) } == 0 {
                dev.set_mem_mhz(mhz as f64);
            }
        }
        if let Some(f) = self.power {
            let mut mw = 0u32;
            if unsafe { f(handle, &mut mw) } == 0 {
                dev.set_power_w(mw as f32 / 1000.0);
            }
        }
        if let Some(f) = self.util {
            let mut u = Utilization::default();
            if unsafe { f(handle, &mut u) } == 0 {
                dev.set_util_pct(u.gpu as f32);
            }
        }
        if let Some(f) = self.memory {
            let mut m = Memory::default();
            if unsafe { f(handle, &mut m) } == 0 {
                dev.vram_used = m.used;
                dev.vram_total = m.total;
            }
        }

        Some(dev)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe { (self.shutdown)() };
    }
}
