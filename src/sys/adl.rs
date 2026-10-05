//! AMD GPU sensors through ADL (`atiadlxx.dll`).
//!
//! ADL is AMD's display library, and its `ADL2_*` entry points are the
//! long-standing way to read a Radeon's temperature, fan, clocks and power
//! without a driver of one's own. The DLL ships with the display driver.
//!
//! Two paths, because AMD has replaced this interface twice:
//!
//! * **PMLog** (`ADL2_New_QueryPMLogData_Get`) on anything from Vega onward: one
//!   call returns an array of sensors, each with a "supported" flag, indexed by a
//!   vendor enum.
//! * **Overdrive temperature** as a fallback for older cards, where the same
//!   figure comes from a single call that returns one integer.
//!
//! ## On trusting an index
//!
//! PMLog identifies a sensor by its position in `ADLSensorType`. The enum grows with
//! every generation, but it grows at the *end*: it is an ABI, and no member has been
//! renumbered, so an index means the same quantity on every driver and one the
//! driver predates comes back with its `supported` flag clear. Each reading is
//! checked against the range its quantity can physically take anyway (see
//! [`GpuSensors`]'s setters), and anything outside is dropped rather than shown --
//! for the driver that sets the flag and then fills in nonsense.
//!
//! This module was written from AMD's published SDK headers with no Radeon to try it
//! on, and it showed: the vendor filter was written against PCI's `0x1002` where ADL
//! reports the decimal 1002, so it matched nothing and every AMD machine looked like
//! a machine with no AMD card in it (see [`is_amd`]). It has since been run against
//! real hardware and reports readings; which of the individual sensors are *right*
//! is still something only a Radeon owner can say.

use super::snapshot::{vendor, GpuSensors};
use super::util::{dll_symbol as symbol, load_dll};
use std::ffi::c_void;
use std::sync::Arc;

/// `ADL_CONTEXT_HANDLE`.
type Context = *mut c_void;
/// Every ADL entry point returns `ADL_OK` (0) or a negative error.
type Status = i32;

const ADL_OK: Status = 0;
const ADL_MAX_PATH: usize = 256;
const PMLOG_MAX_SENSORS: usize = 256;

/// `ADL_MAIN_MALLOC_CALLBACK`: ADL allocates its own output buffers through this.
type MallocCallback = unsafe extern "C" fn(i32) -> *mut c_void;

type FnCreate = unsafe extern "C" fn(MallocCallback, i32, *mut Context) -> Status;
type FnDestroy = unsafe extern "C" fn(Context) -> Status;
type FnNumAdapters = unsafe extern "C" fn(Context, *mut i32) -> Status;
type FnAdapterInfo = unsafe extern "C" fn(Context, *mut AdapterInfo, i32) -> Status;
type FnOverdriveCaps = unsafe extern "C" fn(Context, i32, *mut i32, *mut i32, *mut i32) -> Status;
type FnPmLog = unsafe extern "C" fn(Context, i32, *mut PmLogDataOutput) -> Status;
/// `ADL2_OverdriveN_Temperature_Get(context, adapter, temperatureType, *temp)`
/// and `ADL2_Overdrive6_Temperature_Get(context, adapter, *temp)`: millidegrees.
type FnOdnTemp = unsafe extern "C" fn(Context, i32, i32, *mut i32) -> Status;
type FnOd6Temp = unsafe extern "C" fn(Context, i32, *mut i32) -> Status;

/// `AdapterInfo`, from `adl_structures.h`. Fixed since ADL 3.
#[repr(C)]
#[derive(Clone, Copy)]
struct AdapterInfo {
    size: i32,
    adapter_index: i32,
    udid: [u8; ADL_MAX_PATH],
    bus_number: i32,
    device_number: i32,
    function_number: i32,
    vendor_id: i32,
    adapter_name: [u8; ADL_MAX_PATH],
    display_name: [u8; ADL_MAX_PATH],
    present: i32,
    exist: i32,
    driver_path: [u8; ADL_MAX_PATH],
    driver_path_ext: [u8; ADL_MAX_PATH],
    pnp_string: [u8; ADL_MAX_PATH],
    os_display_index: i32,
}

/// `ADLSingleSensorData`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SensorData {
    supported: i32,
    value: i32,
}

/// `ADLPMLogDataOutput`.
#[repr(C)]
struct PmLogDataOutput {
    size: i32,
    sensors: [SensorData; PMLOG_MAX_SENSORS],
}

const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(offset_of!(AdapterInfo, bus_number) == 264);
    assert!(offset_of!(AdapterInfo, vendor_id) == 276);
    assert!(offset_of!(AdapterInfo, adapter_name) == 280);
    assert!(size_of::<AdapterInfo>() == 1572);
    assert!(size_of::<PmLogDataOutput>() == 4 + 8 * PMLOG_MAX_SENSORS);
};

/// `ADLSensorType`, the members this reads.
///
/// The enum is append-only -- it is an ABI, and every generation has added to the
/// end rather than renumbered -- so an index means the same quantity on every
/// driver, and one the driver has never heard of comes back with its `supported`
/// flag clear. That is what makes reading by index safe; the range checks on
/// [`GpuSensors`]'s setters are the second line, for a driver that sets the flag
/// and then fills in nonsense.
mod sensor {
    pub const CLK_GFXCLK: usize = 1;
    pub const CLK_MEMCLK: usize = 2;
    /// The edge of the die -- what AMD's own control panel calls "GPU
    /// temperature".
    pub const TEMPERATURE_EDGE: usize = 8;
    pub const TEMPERATURE_MEM: usize = 9;
    pub const FAN_RPM: usize = 14;
    pub const FAN_PERCENTAGE: usize = 15;
    pub const INFO_ACTIVITY_GFX: usize = 19;
    /// Whole-ASIC power draw, in watts.
    pub const ASIC_POWER: usize = 23;
    /// The hottest point on the die: AMD's junction temperature, and what the card
    /// actually throttles against.
    pub const TEMPERATURE_HOTSPOT: usize = 27;
    /// The same thing on a chiplet part, where the graphics die has its own.
    /// Vega through RDNA2 answer 27; RDNA3 splits the reading in two and answers
    /// here instead, so both are asked and the first that replies wins.
    pub const TEMPERATURE_HOTSPOT_GCD: usize = 50;
    /// The *processor's* temperature, on an APU where it shares the die.
    pub const TEMPERATURE_CPU: usize = 32;
    /// The processor's power draw, in watts, on the same parts.
    pub const CPU_POWER: usize = 33;
}

/// `ADLODNTemperatureType_Core`.
const ODN_TEMPERATURE_CORE: i32 = 1;

/// Is this `AdapterInfo::iVendorID` an AMD one?
///
/// ADL does not report the PCI vendor id in `iVendorID`. It reports the *decimal*
/// number 1002 where PCI's own id for AMD is `0x1002` — the hex digits read as
/// decimal. AMD's published sample says so outright:
///
/// ```text
/// #define AMDVENDORID (1002)
/// ...
/// if ( adapterActive && adapterInfo.iVendorID == AMDVENDORID )
/// ```
///
/// A filter written against `0x1002` (4098) therefore matches nothing at all, and
/// a Radeon looks exactly like an absent one: no adapters, no context, no sensors,
/// no error. Both encodings are accepted, because which one a given driver returns
/// is not something this code can check from here, and there is no cost to taking
/// either — 0x1002 is AMD's real PCI id, and 1002 decimal (`0x3EA`) belongs to no
/// display vendor.
fn is_amd(vendor_id: i32) -> bool {
    vendor_id == 1002 || vendor_id == vendor::AMD as i32
}

/// ADL asks the caller for an allocator, and refuses to initialize without one.
///
/// Every output structure this module passes in is one it owns, so nothing here
/// receives an ADL-allocated buffer and in practice the callback is never reached.
/// Should a future call take one, it is the caller's to release: ADL's ownership
/// rule is that whatever came through this function goes back through the caller's
/// own free, which means any such buffer must be reclaimed with
/// `Vec::from_raw_parts` at a point that knows its size.
///
/// `extern "C"` rather than `"system"`: ADL declares cdecl, which on x86-64
/// Windows is the only convention there is — the two are the same ABI here, and
/// this app is x64-only.
unsafe extern "C" fn adl_malloc(bytes: i32) -> *mut c_void {
    if bytes <= 0 {
        return std::ptr::null_mut();
    }
    let mut v: Vec<u8> = Vec::with_capacity(bytes as usize);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p.cast()
}

pub struct Adl {
    context: Context,
    destroy: FnDestroy,
    overdrive_caps: Option<FnOverdriveCaps>,
    pm_log: Option<FnPmLog>,
    odn_temp: Option<FnOdnTemp>,
    od6_temp: Option<FnOd6Temp>,
    /// `(ADL adapter index, name)` for each distinct AMD GPU, in bus order.
    adapters: Vec<(i32, Arc<str>)>,
}

// The context is an opaque driver pointer used only from the sampler thread.
unsafe impl Send for Adl {}

impl Adl {
    /// `None` when there is no AMD driver, or when it exposes no usable adapter.
    pub fn load() -> Option<Self> {
        // The 64-bit library. `atiadlxy.dll` is its 32-bit sibling, which a
        // 64-bit process cannot load.
        let module = load_dll("atiadlxx.dll")?;

        let create: FnCreate = unsafe { symbol(module, b"ADL2_Main_Control_Create\0")? };
        let destroy: FnDestroy = unsafe { symbol(module, b"ADL2_Main_Control_Destroy\0")? };
        let num_adapters: FnNumAdapters =
            unsafe { symbol(module, b"ADL2_Adapter_NumberOfAdapters_Get\0")? };
        let adapter_info: FnAdapterInfo =
            unsafe { symbol(module, b"ADL2_Adapter_AdapterInfo_Get\0")? };

        let mut context: Context = std::ptr::null_mut();
        // `iEnumConnectedAdapters = 1`: adapters that are physically present and
        // enabled, rather than every adapter that has ever been in this machine.
        if unsafe { create(adl_malloc, 1, &mut context) } != ADL_OK || context.is_null() {
            return None;
        }

        let adapters = unsafe { Self::adapters(context, num_adapters, adapter_info) };
        if adapters.is_empty() {
            unsafe { destroy(context) };
            return None;
        }

        Some(Self {
            context,
            destroy,
            overdrive_caps: unsafe { symbol(module, b"ADL2_Overdrive_Caps\0") },
            pm_log: unsafe { symbol(module, b"ADL2_New_QueryPMLogData_Get\0") },
            odn_temp: unsafe { symbol(module, b"ADL2_OverdriveN_Temperature_Get\0") },
            od6_temp: unsafe { symbol(module, b"ADL2_Overdrive6_Temperature_Get\0") },
            adapters,
        })
    }

    /// One entry per physical AMD GPU.
    ///
    /// ADL reports an "adapter" per display output, so a card with three monitors
    /// appears three times. They share a PCI bus/device pair, which is what
    /// distinguishes a second card from a second output on the first.
    ///
    /// # Safety
    /// `context` must be a live ADL context and the two function pointers its own.
    unsafe fn adapters(
        context: Context,
        num_adapters: FnNumAdapters,
        adapter_info: FnAdapterInfo,
    ) -> Vec<(i32, Arc<str>)> {
        let mut count = 0i32;
        if num_adapters(context, &mut count) != ADL_OK || count <= 0 {
            return Vec::new();
        }
        let count = count.min(64) as usize;

        let mut info: Vec<AdapterInfo> = vec![std::mem::zeroed(); count];
        let bytes = (count * std::mem::size_of::<AdapterInfo>()) as i32;
        if adapter_info(context, info.as_mut_ptr(), bytes) != ADL_OK {
            return Vec::new();
        }

        let mut seen: Vec<(i32, i32)> = Vec::new();
        let mut out = Vec::new();
        for a in &info {
            // The vendor and a non-empty UDID are the whole test. `iPresent` is
            // deliberately not part of it: `ADL2_Main_Control_Create` has already
            // been asked for adapters that are present and enabled, and on a
            // hybrid-graphics laptop the discrete Radeon -- the one whose sensors
            // are worth reading -- can report `iPresent = 0` simply because no
            // display hangs off it. An empty UDID is how ADL leaves a slot it has
            // nothing to say about.
            if !is_amd(a.vendor_id) || cstr(&a.udid).is_empty() {
                continue;
            }
            let bus = (a.bus_number, a.device_number);
            if seen.contains(&bus) {
                continue;
            }
            seen.push(bus);
            let name = cstr(&a.adapter_name);
            out.push((a.adapter_index, Arc::from(name.as_str())));
        }
        out
    }

    pub fn device_count(&self) -> u32 {
        self.adapters.len() as u32
    }

    pub fn read(&self, i: u32) -> Option<GpuSensors> {
        let (adapter, name) = self.adapters.get(i as usize)?.clone();
        let mut out = GpuSensors {
            vendor: vendor::AMD,
            ordinal: i,
            source: "ADL",
            name,
            ..Default::default()
        };

        if !self.read_pmlog(adapter, &mut out) {
            self.read_overdrive_temperature(adapter, &mut out);
        }
        Some(out)
    }

    /// The modern path. Returns false when this adapter has no PMLog interface.
    fn read_pmlog(&self, adapter: i32, out: &mut GpuSensors) -> bool {
        let Some(pm_log) = self.pm_log else {
            return false;
        };
        // SAFETY: a zeroed `PmLogDataOutput` is a valid one -- every sensor reads
        // as unsupported until the driver says otherwise.
        let mut data: PmLogDataOutput = unsafe { std::mem::zeroed() };
        if unsafe { pm_log(self.context, adapter, &mut data) } != ADL_OK {
            return false;
        }

        let at = |i: usize| -> Option<i32> {
            let s = data.sensors.get(i)?;
            (s.supported != 0).then_some(s.value)
        };

        // Checked before anything is committed: a memory temperature outside the
        // range a die can physically be in means the enum has shifted underneath
        // these indices, and then every other reading in the set is suspect too.
        // Read for that reason alone -- it is not shown anywhere.
        if let Some(v) = at(sensor::TEMPERATURE_MEM) {
            if !(-40..=150).contains(&v) {
                return false;
            }
        }

        let mut any = false;
        if let Some(v) = at(sensor::TEMPERATURE_EDGE) {
            out.set_temp(v as f32);
            any |= out.temp_c.is_some();
        }
        if let Some(v) =
            at(sensor::TEMPERATURE_HOTSPOT).or_else(|| at(sensor::TEMPERATURE_HOTSPOT_GCD))
        {
            out.set_hotspot(v as f32);
            any |= out.hotspot_c.is_some();
        }
        if let Some(v) = at(sensor::FAN_RPM) {
            out.set_fan_rpm(v as i64);
            any |= out.fan_rpm.is_some();
        }
        if let Some(v) = at(sensor::FAN_PERCENTAGE) {
            out.set_fan_pct(v as f32);
            any |= out.fan_pct.is_some();
        }
        if let Some(v) = at(sensor::CLK_GFXCLK) {
            out.set_core_mhz(v as f64);
            any |= out.core_mhz.is_some();
        }
        if let Some(v) = at(sensor::CLK_MEMCLK) {
            out.set_mem_mhz(v as f64);
            any |= out.mem_mhz.is_some();
        }
        if let Some(v) = at(sensor::ASIC_POWER) {
            out.set_power_w(v as f32);
            any |= out.power_w.is_some();
        }
        if let Some(v) = at(sensor::INFO_ACTIVITY_GFX) {
            out.set_util_pct(v as f32);
            any |= out.util_pct.is_some();
        }
        // The processor's own readings, which an APU's sensor set carries because the
        // two are one die. A discrete card leaves both unsupported, and then the CPU
        // temperature comes from the firmware's thermal zone as it does on Intel.
        if let Some(v) = at(sensor::TEMPERATURE_CPU) {
            let c = v as f32;
            if (-40.0..=150.0).contains(&c) {
                out.cpu_temp_c = Some(c);
                any = true;
            }
        }
        if let Some(v) = at(sensor::CPU_POWER) {
            let w = v as f32;
            if (0.0..=1000.0).contains(&w) {
                out.cpu_power_w = Some(w);
                any = true;
            }
        }
        any
    }

    /// Temperature only, for cards older than PMLog. Overdrive's version tells
    /// which of the two calls the driver answers.
    fn read_overdrive_temperature(&self, adapter: i32, out: &mut GpuSensors) {
        let mut version = 0i32;
        if let Some(caps) = self.overdrive_caps {
            let (mut supported, mut enabled) = (0i32, 0i32);
            if unsafe {
                caps(
                    self.context,
                    adapter,
                    &mut supported,
                    &mut enabled,
                    &mut version,
                )
            } != ADL_OK
            {
                return;
            }
            if supported == 0 {
                return;
            }
        }

        // Both report millidegrees.
        let millidegrees = match (version, self.odn_temp, self.od6_temp) {
            (7.., Some(f), _) => {
                let mut t = 0i32;
                (unsafe { f(self.context, adapter, ODN_TEMPERATURE_CORE, &mut t) } == ADL_OK)
                    .then_some(t)
            }
            (_, _, Some(f)) => {
                let mut t = 0i32;
                (unsafe { f(self.context, adapter, &mut t) } == ADL_OK).then_some(t)
            }
            _ => None,
        };
        if let Some(m) = millidegrees {
            out.set_temp(m as f32 / 1000.0);
        }
    }
}

impl Drop for Adl {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.context) };
    }
}

/// Decode one of ADL's fixed-size `char` fields.
fn cstr(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one thing testable without a Radeon: that the structure this asks the
    /// driver to fill is the size the driver expects. A short `AdapterInfo` array
    /// would have ADL write past the end of it.
    #[test]
    fn adapter_info_matches_the_published_layout() {
        assert_eq!(std::mem::size_of::<AdapterInfo>(), 1572);
        assert_eq!(std::mem::size_of::<SensorData>(), 8);
        assert_eq!(std::mem::size_of::<PmLogDataOutput>(), 2052);
    }

    /// The filter has to accept what ADL actually puts in `iVendorID`, which is
    /// 1002 in decimal -- not the `0x1002` that PCI and the rest of this app use.
    ///
    /// This is the whole of what went wrong the first time: comparing against
    /// `0x1002` matched no adapter, so `load` found none and returned `None`, and a
    /// machine with a Radeon in it behaved in every visible way like a machine
    /// without one.
    #[test]
    fn accepts_the_vendor_id_adl_reports_not_the_one_pci_does() {
        assert!(is_amd(1002), "the number AMD's own sample compares against");
        assert!(is_amd(vendor::AMD as i32), "and the true PCI id");
        assert!(!is_amd(vendor::NVIDIA as i32));
        assert!(!is_amd(vendor::INTEL as i32));
        assert!(!is_amd(0), "a slot ADL left empty");
    }

    /// Enumeration, driven by a stand-in for ADL.
    ///
    /// `adapters` takes its two entry points as arguments, so the whole walk can be
    /// run against a buffer laid out the way the driver lays one out -- the real
    /// `AdapterInfo`, at the real offsets -- on a machine with no Radeon in it.
    /// This is the test that fails outright if the vendor filter is wrong, rather
    /// than only if a reader notices the predicate is.
    #[test]
    fn keeps_one_entry_per_card_and_drops_what_is_not_one() {
        /// Four adapters, as ADL would hand them over: one Radeon, the same Radeon
        /// again for its second display output, an Intel adapter, and a slot ADL
        /// filled in with nothing.
        unsafe extern "C" fn count(_: Context, out: *mut i32) -> Status {
            unsafe { *out = 4 };
            ADL_OK
        }
        unsafe extern "C" fn info(_: Context, out: *mut AdapterInfo, bytes: i32) -> Status {
            assert_eq!(
                bytes as usize,
                4 * std::mem::size_of::<AdapterInfo>(),
                "a short buffer would have ADL write past the end of it"
            );
            let slots = unsafe { std::slice::from_raw_parts_mut(out, 4) };
            slots.fill(unsafe { std::mem::zeroed() });

            fill(&mut slots[0].udid, "PCI_VEN_1002&DEV_744C_0");
            slots[0].adapter_index = 5; // not its position in the array
            slots[0].vendor_id = 1002;
            slots[0].bus_number = 3;
            fill(&mut slots[0].adapter_name, "Radeon RX 7900 XTX");
            // Zero on purpose: a hybrid-graphics laptop reports this for the
            // discrete card when no display hangs off it, and that card is
            // precisely the one whose temperature is worth reading.
            slots[0].present = 0;

            // Same card, second output: same bus and device, different index.
            slots[1] = slots[0];
            slots[1].adapter_index = 6;

            fill(&mut slots[2].udid, "PCI_VEN_8086&DEV_7D67_0");
            slots[2].adapter_index = 7;
            slots[2].vendor_id = 0x8086;
            slots[2].bus_number = 0;
            fill(&mut slots[2].adapter_name, "Intel(R) Graphics");
            slots[2].present = 1;

            // An AMD vendor id but no UDID: a slot, not an adapter.
            slots[3].adapter_index = 8;
            slots[3].vendor_id = 1002;
            slots[3].bus_number = 9;
            ADL_OK
        }
        fn fill(dst: &mut [u8], s: &str) {
            let b = s.as_bytes();
            dst[..b.len()].copy_from_slice(b);
            dst[b.len()] = 0;
        }

        let found = unsafe { Adl::adapters(std::ptr::null_mut(), count, info) };
        assert_eq!(found.len(), 1, "one entry per physical card: {found:?}");
        assert_eq!(
            found[0].0, 5,
            "the index ADL gave it, not the array position"
        );
        assert_eq!(&*found[0].1, "Radeon RX 7900 XTX");
    }

    /// A sensor the driver did not fill in must not be read as a zero reading.
    #[test]
    fn an_unsupported_sensor_is_absent_not_zero() {
        let data: PmLogDataOutput = unsafe { std::mem::zeroed() };
        let s = data.sensors[sensor::TEMPERATURE_EDGE];
        assert_eq!(s.supported, 0);
        assert!(
            (s.supported != 0).then_some(s.value).is_none(),
            "the supported flag is what gates a reading"
        );
    }
}
