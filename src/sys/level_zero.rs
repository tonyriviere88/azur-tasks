//! Intel GPU sensors through Level Zero Sysman (`ze_loader.dll`).
//!
//! Sysman is the system-management half of oneAPI Level Zero, and Intel's
//! supported way to read a GPU's temperature, fan, frequency, memory and power.
//! The loader ships with the graphics driver, so a machine with no Intel driver
//! simply has no Intel sensors.
//!
//! Every reading is enumerated rather than asked for: a device publishes a list
//! of temperature sensors, a list of frequency domains, and so on, each with
//! properties saying what it measures. An *integrated* GPU usually publishes very
//! few — it has no temperature of its own (it shares the CPU package's, which the
//! ACPI thermal zones already report) and no fan of its own (the chassis fan
//! belongs to the embedded controller). Frequency and memory it does publish.
//!
//! Structures are passed zeroed, with the `stype` tag left at 0. Intel's
//! implementation fills these on output and does not check the tag; where a
//! future driver does, the call fails and the reading shows as absent rather than
//! as a wrong number.

use super::snapshot::{vendor, GpuSensors};
use super::util::{dll_symbol as symbol, load_dll};
use std::ffi::c_void;
use std::sync::Arc;

type Handle = *mut c_void;
/// `ze_result_t`, 0 on success.
type Result = u32;

type FnInit = unsafe extern "system" fn(u32) -> Result;
/// `zesDriverGet`, and every `zesDeviceEnum*` after it: the two-call pattern --
/// once with a null array to learn the count, once to fill it. `zesDriverGet`
/// takes no handle; the rest take the thing being enumerated.
type FnDriverGet = unsafe extern "system" fn(*mut u32, *mut Handle) -> Result;
type FnEnum = unsafe extern "system" fn(Handle, *mut u32, *mut Handle) -> Result;
type FnTempProps = unsafe extern "system" fn(Handle, *mut TempProperties) -> Result;
type FnTempState = unsafe extern "system" fn(Handle, *mut f64) -> Result;
type FnFanState = unsafe extern "system" fn(Handle, u32, *mut i32) -> Result;
type FnFreqProps = unsafe extern "system" fn(Handle, *mut FreqProperties) -> Result;
type FnFreqState = unsafe extern "system" fn(Handle, *mut FreqState) -> Result;
type FnMemState = unsafe extern "system" fn(Handle, *mut MemState) -> Result;
type FnEnergy = unsafe extern "system" fn(Handle, *mut EnergyCounter) -> Result;

/// `zes_temp_properties_t`.
#[repr(C)]
#[derive(Default)]
struct TempProperties {
    stype: u32,
    p_next: *const c_void,
    /// `zes_temp_sensors_t`.
    kind: u32,
    on_subdevice: u8,
    subdevice_id: u32,
    max_temperature: f64,
    is_critical_supported: u8,
    is_threshold1_supported: u8,
    is_threshold2_supported: u8,
}

/// `zes_freq_properties_t`.
#[repr(C)]
#[derive(Default)]
struct FreqProperties {
    stype: u32,
    p_next: *const c_void,
    /// `zes_freq_domain_t`: 0 = GPU, 1 = memory.
    domain: u32,
    on_subdevice: u8,
    subdevice_id: u32,
    can_control: u8,
    is_throttle_event_supported: u8,
    min: f64,
    max: f64,
}

/// `zes_freq_state_t`. `actual` is what the domain is running at, in MHz;
/// `request` is what was asked for, which on an idle part is often much higher.
#[repr(C)]
#[derive(Default)]
struct FreqState {
    stype: u32,
    p_next: *const c_void,
    current_voltage: f64,
    request: f64,
    tdp: f64,
    efficient: f64,
    actual: f64,
    throttle_reasons: u32,
}

/// `zes_mem_state_t`, in bytes.
#[repr(C)]
#[derive(Default)]
struct MemState {
    stype: u32,
    p_next: *const c_void,
    health: u32,
    free: u64,
    size: u64,
}

/// `zes_power_energy_counter_t`: microjoules and microseconds, both monotonic.
/// Power is the ratio of their deltas -- there is no "watts now" call.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct EnergyCounter {
    stype: u32,
    p_next: *const c_void,
    energy: u64,
    timestamp: u64,
}

// The layouts above are C structs from `zes_api.h`, and a wrong one would read a
// neighbouring field as a temperature. Pinned, the way `nt.rs` pins its own.
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(offset_of!(TempProperties, kind) == 16);
    assert!(offset_of!(TempProperties, max_temperature) == 32);
    assert!(size_of::<TempProperties>() == 48);
    assert!(offset_of!(FreqProperties, domain) == 16);
    assert!(offset_of!(FreqProperties, min) == 32);
    assert!(size_of::<FreqProperties>() == 48);
    assert!(offset_of!(FreqState, request) == 24);
    assert!(offset_of!(FreqState, actual) == 48);
    assert!(size_of::<FreqState>() == 64);
    assert!(offset_of!(MemState, free) == 24);
    assert!(size_of::<MemState>() == 40);
    assert!(offset_of!(EnergyCounter, energy) == 16);
    assert!(size_of::<EnergyCounter>() == 32);
};

/// `zes_temp_sensors_t`: the whole device, rather than one part of it.
const TEMP_SENSORS_GLOBAL: u32 = 0;
/// The GPU core.
const TEMP_SENSORS_GPU: u32 = 1;
/// `zes_freq_domain_t`.
const FREQ_DOMAIN_GPU: u32 = 0;
const FREQ_DOMAIN_MEMORY: u32 = 1;
/// `zes_fan_speed_units_t`.
const FAN_UNITS_RPM: u32 = 0;
const FAN_UNITS_PERCENT: u32 = 1;

pub struct LevelZero {
    enum_temp: Option<FnEnum>,
    temp_props: Option<FnTempProps>,
    temp_state: Option<FnTempState>,
    enum_fans: Option<FnEnum>,
    fan_state: Option<FnFanState>,
    enum_freq: Option<FnEnum>,
    freq_props: Option<FnFreqProps>,
    freq_state: Option<FnFreqState>,
    enum_mem: Option<FnEnum>,
    mem_state: Option<FnMemState>,
    enum_power: Option<FnEnum>,
    energy: Option<FnEnergy>,
    /// One handle per Intel device, in the driver's order.
    devices: Vec<Handle>,
    /// Previous energy reading per device, for the power delta.
    prev_energy: Vec<Option<EnergyCounter>>,
}

// The handles are opaque driver pointers, used only from the sampler thread; the
// probe is created there and never shared.
unsafe impl Send for LevelZero {}

impl LevelZero {
    /// `None` when there is no Intel driver, or when it predates `zesInit`.
    pub fn load() -> Option<Self> {
        let module = load_dll("ze_loader.dll")?;

        // `zesInit` rather than `zeInit`: the dedicated sysman initializer, which
        // does not need the `ZES_ENABLE_SYSMAN` environment variable that the old
        // path required -- and setting an environment variable to read a
        // temperature would leak into every child process the app starts.
        let init: FnInit = unsafe { symbol(module, b"zesInit\0")? };
        let driver_get: FnDriverGet = unsafe { symbol(module, b"zesDriverGet\0")? };
        let device_get: FnEnum = unsafe { symbol(module, b"zesDeviceGet\0")? };

        if unsafe { init(0) } != 0 {
            return None;
        }

        // One driver per installed implementation; devices are enumerated per
        // driver, and in practice there is exactly one Intel driver.
        let drivers = unsafe { list(|n, p| driver_get(n, p)) };
        let mut devices = Vec::new();
        for d in drivers {
            devices.extend(unsafe { list(|n, p| device_get(d, n, p)) });
        }
        if devices.is_empty() {
            return None;
        }

        let prev_energy = vec![None; devices.len()];
        Some(Self {
            enum_temp: unsafe { symbol(module, b"zesDeviceEnumTemperatureSensors\0") },
            temp_props: unsafe { symbol(module, b"zesTemperatureGetProperties\0") },
            temp_state: unsafe { symbol(module, b"zesTemperatureGetState\0") },
            enum_fans: unsafe { symbol(module, b"zesDeviceEnumFans\0") },
            fan_state: unsafe { symbol(module, b"zesFanGetState\0") },
            enum_freq: unsafe { symbol(module, b"zesDeviceEnumFrequencyDomains\0") },
            freq_props: unsafe { symbol(module, b"zesFrequencyGetProperties\0") },
            freq_state: unsafe { symbol(module, b"zesFrequencyGetState\0") },
            enum_mem: unsafe { symbol(module, b"zesDeviceEnumMemoryModules\0") },
            mem_state: unsafe { symbol(module, b"zesMemoryGetState\0") },
            enum_power: unsafe { symbol(module, b"zesDeviceEnumPowerDomains\0") },
            energy: unsafe { symbol(module, b"zesPowerGetEnergyCounter\0") },
            devices,
            prev_energy,
        })
    }

    pub fn device_count(&self) -> u32 {
        self.devices.len() as u32
    }

    pub fn read(&mut self, i: u32) -> Option<GpuSensors> {
        let device = *self.devices.get(i as usize)?;
        let mut out = GpuSensors {
            vendor: vendor::INTEL,
            ordinal: i,
            source: "Level Zero",
            // Left to the caller: sysman's own device name lives in a large
            // versioned structure, and DXGI already knows what this adapter is
            // called.
            name: Arc::from(""),
            ..Default::default()
        };

        self.read_temperature(device, &mut out);
        self.read_fan(device, &mut out);
        self.read_frequency(device, &mut out);
        self.read_memory(device, &mut out);
        self.read_power(device, i as usize, &mut out);

        Some(out)
    }

    /// The GPU core sensor if the device has one, else the device-wide one.
    fn read_temperature(&self, device: Handle, out: &mut GpuSensors) {
        let (Some(enum_fn), Some(props_fn), Some(state_fn)) =
            (self.enum_temp, self.temp_props, self.temp_state)
        else {
            return;
        };
        let mut best: Option<(u32, f64)> = None;
        for h in unsafe { list(|n, p| enum_fn(device, n, p)) } {
            let mut props = TempProperties::default();
            if unsafe { props_fn(h, &mut props) } != 0 {
                continue;
            }
            if props.kind != TEMP_SENSORS_GPU && props.kind != TEMP_SENSORS_GLOBAL {
                continue;
            }
            let mut celsius = 0.0f64;
            if unsafe { state_fn(h, &mut celsius) } != 0 {
                continue;
            }
            // GPU beats GLOBAL; between two of a kind, the hotter reading.
            let better = match best {
                None => true,
                Some((kind, c)) => (props.kind, celsius) > (kind, c),
            };
            if better {
                best = Some((props.kind, celsius));
            }
        }
        if let Some((_, celsius)) = best {
            out.set_temp(celsius as f32);
        }
    }

    fn read_fan(&self, device: Handle, out: &mut GpuSensors) {
        let (Some(enum_fn), Some(state_fn)) = (self.enum_fans, self.fan_state) else {
            return;
        };
        for h in unsafe { list(|n, p| enum_fn(device, n, p)) } {
            let mut speed = -1i32;
            if unsafe { state_fn(h, FAN_UNITS_PERCENT, &mut speed) } == 0 && speed >= 0 {
                out.set_fan_pct(speed as f32);
            }
            let mut rpm = -1i32;
            if unsafe { state_fn(h, FAN_UNITS_RPM, &mut rpm) } == 0 && rpm > 0 {
                out.set_fan_rpm(rpm as i64);
            }
            if out.fan_pct.is_some() || out.fan_rpm.is_some() {
                return;
            }
        }
    }

    fn read_frequency(&self, device: Handle, out: &mut GpuSensors) {
        let (Some(enum_fn), Some(props_fn), Some(state_fn)) =
            (self.enum_freq, self.freq_props, self.freq_state)
        else {
            return;
        };
        for h in unsafe { list(|n, p| enum_fn(device, n, p)) } {
            let mut props = FreqProperties::default();
            if unsafe { props_fn(h, &mut props) } != 0 {
                continue;
            }
            let mut state = FreqState::default();
            if unsafe { state_fn(h, &mut state) } != 0 {
                continue;
            }
            // `actual` is -1 when the domain cannot report it; `request` is what
            // the driver asked for, which is the honest fallback.
            let mhz = if state.actual > 0.0 {
                state.actual
            } else {
                state.request
            };
            match props.domain {
                FREQ_DOMAIN_GPU => out.set_core_mhz(mhz),
                FREQ_DOMAIN_MEMORY => out.set_mem_mhz(mhz),
                _ => {}
            }
        }
    }

    /// Summed across modules: a discrete Intel board reports one per memory
    /// controller, and their total is the board's.
    fn read_memory(&self, device: Handle, out: &mut GpuSensors) {
        let (Some(enum_fn), Some(state_fn)) = (self.enum_mem, self.mem_state) else {
            return;
        };
        let mut used = 0u64;
        let mut total = 0u64;
        for h in unsafe { list(|n, p| enum_fn(device, n, p)) } {
            let mut state = MemState::default();
            if unsafe { state_fn(h, &mut state) } != 0 || state.size == 0 {
                continue;
            }
            total = total.saturating_add(state.size);
            used = used.saturating_add(state.size.saturating_sub(state.free));
        }
        if total > 0 {
            out.vram_used = used;
            out.vram_total = total;
        }
    }

    /// Watts from the difference between two energy readings.
    ///
    /// Sysman exposes a monotonic joule counter, not a power figure, so the first
    /// tick after start-up reports nothing and every tick after it reports the
    /// average over the interval -- which is what a one-second sample means
    /// anyway.
    fn read_power(&mut self, device: Handle, index: usize, out: &mut GpuSensors) {
        let (Some(enum_fn), Some(energy_fn)) = (self.enum_power, self.energy) else {
            return;
        };
        // The first domain is the whole card; later ones are its parts.
        let Some(&h) = unsafe { list(|n, p| enum_fn(device, n, p)) }.first() else {
            return;
        };
        let mut now = EnergyCounter::default();
        if unsafe { energy_fn(h, &mut now) } != 0 {
            return;
        }
        if let Some(prev) = self.prev_energy.get(index).copied().flatten() {
            let dj = now.energy.wrapping_sub(prev.energy) as f64;
            let dt = now.timestamp.wrapping_sub(prev.timestamp) as f64;
            // Microjoules over microseconds is watts. A zero or backwards
            // interval means the counter reset; skip rather than divide.
            if dt > 0.0 && dj >= 0.0 {
                out.set_power_w((dj / dt) as f32);
            }
        }
        if let Some(slot) = self.prev_energy.get_mut(index) {
            *slot = Some(now);
        }
    }
}

/// The two-call enumeration every `zes*Get`/`zes*Enum` uses: ask for the count,
/// then fill a buffer of that size.
///
/// # Safety
/// `f` must be a sysman enumerator with the `(count, array)` contract.
unsafe fn list(mut f: impl FnMut(*mut u32, *mut Handle) -> Result) -> Vec<Handle> {
    let mut count = 0u32;
    if f(&mut count, std::ptr::null_mut()) != 0 || count == 0 {
        return Vec::new();
    }
    // A device with thousands of sensors is a driver bug, not a machine.
    count = count.min(64);
    let mut handles = vec![std::ptr::null_mut(); count as usize];
    if f(&mut count, handles.as_mut_ptr()) != 0 {
        return Vec::new();
    }
    handles.truncate(count as usize);
    handles.retain(|h| !h.is_null());
    handles
}
