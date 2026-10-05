//! What the hardware says about itself: temperatures, fans, clocks.
//!
//! Three sources, none of which needs administrator rights:
//!
//! * **ACPI thermal zones**, through the `Thermal Zone Information` performance
//!   counter set. The firmware publishes one zone per sensor it knows about, in
//!   Kelvin. This is the same data as `MSAcpi_ThermalZoneTemperature` in WMI,
//!   which *does* require elevation -- the counter does not.
//! * **A GPU's own vendor interface**, one per vendor, each loaded only if its
//!   driver is installed: [`super::nvml`] for NVIDIA, [`super::level_zero`] for
//!   Intel, [`super::adl`] for AMD. There is no vendor-neutral way to read a GPU's
//!   temperature -- D3D and the performance counters know about load and memory,
//!   never about heat.
//! * **`CallNtPowerInformation`** for what each core is actually clocked at,
//!   which no counter reports and the registry only knows the nominal value of.
//!
//! Everything is optional. A machine that publishes no thermal zones and has no
//! NVIDIA driver reports no temperatures, and the tab says so rather than drawing
//! a gauge at zero -- a sensor reading of "0 °C" is a lie, "—" is not.

use super::gpu_backend::{GpuBackend, BACKENDS};
use super::pdh::{Counter, Query};
use super::snapshot::{Sensors, ThermalZone};
use super::util::load_dll;
use std::sync::Arc;

/// `\Thermal Zone Information(*)\Temperature`, in Kelvin.
const PATH_THERMAL: &str = r"\Thermal Zone Information(*)\Temperature";

/// `\Energy Meter(*)\Power`, in milliwatts.
///
/// The Energy Metering Interface, which on an Intel part surfaces the RAPL rails --
/// the same registers a power-monitoring tool reads through a driver, published here
/// by Windows and readable by anyone. No elevation, no driver, no MSR access.
const PATH_ENERGY: &str = r"\Energy Meter(*)\Power";

pub struct SensorProbe {
    /// One query for every performance counter here: PDH collects them together, and
    /// a second query would be a second round trip for no reason.
    pdh: Option<Query>,
    thermal: Option<Counter>,
    energy: Option<Counter>,
    /// Whichever vendor libraries this machine actually has, in [`BACKENDS`] order
    /// — which is the order their readings are appended in, and so the order
    /// `Sensors::gpus` comes out in every tick.
    gpus: Vec<Box<dyn GpuBackend>>,
    items: Vec<(String, f64)>,
    /// Settled at start-up, since that is when the libraries are bound.
    notes: Vec<(u32, Arc<str>)>,
}

impl SensorProbe {
    pub fn new() -> Self {
        let pdh = Query::open();
        let (thermal, energy) = match &pdh {
            Some(q) => {
                let pair = (Counter::add(q, PATH_THERMAL), Counter::add(q, PATH_ENERGY));
                // Prime them: the first collection of a multi-instance counter only
                // establishes the instance list.
                q.collect();
                pair
            }
            None => (None, None),
        };

        // Bound once, at start-up: a driver does not appear while the app is
        // running, and a failed `LoadLibrary` per second would be a waste. This
        // walks the registry rather than naming the vendors, so a fourth of them is
        // a row in `gpu_backend.rs` and nothing here.
        let mut gpus = Vec::new();
        let mut notes = Vec::new();
        for b in &BACKENDS {
            match (b.load)() {
                Some(backend) => gpus.push(backend),
                None => silent(&mut notes, b.vendor, b.dll, b.api),
            }
        }

        Self {
            pdh,
            thermal,
            energy,
            gpus,
            items: Vec::new(),
            notes,
        }
    }

    pub fn sample(&mut self, out: &mut Sensors) {
        // One collection for every counter in the query, before any of them is read.
        let collected = self.pdh.as_ref().is_some_and(|q| q.collect());

        out.zones.clear();
        if let Some(counter) = self.thermal.as_mut() {
            if collected {
                counter.read(&mut self.items);
                for (instance, kelvin) in &self.items {
                    // Firmware publishes zones it has no reading for, and says so
                    // with a placeholder: 0 K, or exactly 273.15 K -- which would
                    // otherwise show as a chipset sitting at 0 C inside a running
                    // machine. Anything past the melting point is a stub too.
                    if !(200.0..500.0).contains(kelvin) || (*kelvin - 273.15).abs() < 0.5 {
                        continue;
                    }
                    out.zones.push(ThermalZone {
                        label: zone_label(instance),
                        celsius: (*kelvin - 273.15) as f32,
                    });
                }
                // Left in the firmware's own order, arbitrary as it is: the
                // order has to be the same every tick for a zone's history to
                // stay its own, and sorting by temperature reorders the list
                // whenever two zones cross.
            }
        }

        out.gpus.clear();
        for backend in &mut self.gpus {
            for i in 0..backend.device_count() {
                out.gpus.extend(backend.read(i));
            }
        }

        out.notes.clone_from(&self.notes);

        let (cur, max) = cpu_clock();
        out.cpu_mhz = cur;
        out.cpu_max_mhz = max;

        // The package's own sensor if anything has one, and the firmware's thermal
        // zone otherwise. A zone is the firmware's opinion about a region of the
        // board; a package sensor is the die, and where both exist the die wins.
        out.cpu_temp_c = out
            .gpus
            .iter()
            .find_map(|g| g.cpu_temp_c)
            .or_else(|| out.zone("CPU"));
        // RAPL first: it is a direct reading of the package's own energy counters.
        // ADL's figure is the same quantity on an APU, and the only one on a machine
        // with no Energy Metering Interface.
        out.cpu_power_w = self
            .cpu_power(collected)
            .or_else(|| out.gpus.iter().find_map(|g| g.cpu_power_w));
    }

    /// CPU package power in watts, from the Energy Metering Interface.
    ///
    /// Instances are RAPL rails: `RAPL_Package0_PKG` is the whole package, `_PP0` its
    /// cores, `_PP1` the integrated graphics, `_DRAM` the memory controller. The
    /// package is what a reader means by "CPU power" — it is what the part's power
    /// limit applies to — so the packages are summed and the sub-rails left alone.
    /// Cores are the fallback for a part that publishes no package rail; they are an
    /// undercount, but an undercount of the right thing.
    ///
    /// `_Total` is skipped deliberately. It is PDH's own sum across every instance,
    /// which adds the package to its own parts and roughly doubles the answer.
    fn cpu_power(&mut self, collected: bool) -> Option<f32> {
        let counter = self.energy.as_mut()?;
        if !collected {
            return None;
        }
        counter.read(&mut self.items);
        let sum = |suffix: &str| -> Option<f32> {
            let mut total = 0.0f64;
            let mut found = false;
            for (name, milliwatts) in self.items.iter() {
                if name.ends_with(suffix) {
                    total += milliwatts;
                    found = true;
                }
            }
            found.then_some((total / 1000.0) as f32)
        };
        let watts = sum("_PKG").or_else(|| sum("_PP0"))?;
        // A machine with the counter set but no metering hardware behind it reads
        // zero on every rail, which is not a processor drawing no power.
        (watts > 0.05 && watts < 1000.0).then_some(watts)
    }
}

/// Note a vendor library that is installed on this machine and still said nothing.
///
/// Called only for a backend that failed to bind. The library being *absent* is the
/// ordinary case and gets no note -- most machines have one vendor's card, so two of
/// the three are expected to be missing. Present and mute is the case worth
/// reporting: it means the interface refused to start, or started and enumerated no
/// adapter, and a reader staring at a temperature that is not there has no other way
/// to tell that apart from having no such card.
fn silent(out: &mut Vec<(u32, Arc<str>)>, vendor: u32, dll: &str, api: &str) {
    // `load_dll` on something already loaded only takes a reference to it, and
    // these stay loaded for the life of the process either way.
    if load_dll(dll).is_none() {
        return;
    }
    out.push((
        vendor,
        Arc::from(
            format!("{dll} is installed, but {api} reported no adapter it could read.").as_str(),
        ),
    ));
}

/// Current and maximum CPU clock in MHz, averaged over the logical processors.
///
/// `CurrentMhz` is per-core and they rarely agree, so the average is what a
/// single "CPU clock" reading can honestly be. `MaxMhz` is the nominal figure --
/// the current clock goes above it whenever the part boosts, which is why the
/// gauge below scales to the highest reading rather than to this.
fn cpu_clock() -> (u32, u32) {
    use windows_sys::Win32::System::Power::{
        CallNtPowerInformation, ProcessorInformation, PROCESSOR_POWER_INFORMATION,
    };

    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    // Room for more processors than were reported, in case the group layout
    // disagrees: a short buffer is refused outright.
    let mut buf = vec![PROCESSOR_POWER_INFORMATION::default(); n + 8];
    let bytes = (buf.len() * std::mem::size_of::<PROCESSOR_POWER_INFORMATION>()) as u32;
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            std::ptr::null(),
            0,
            buf.as_mut_ptr().cast(),
            bytes,
        )
    };
    if status != 0 {
        return (0, 0);
    }

    let live: Vec<&PROCESSOR_POWER_INFORMATION> =
        buf.iter().filter(|p| p.MaxMhz > 0).take(n).collect();
    if live.is_empty() {
        return (0, 0);
    }
    let sum: u64 = live.iter().map(|p| p.CurrentMhz as u64).sum();
    let max = live.iter().map(|p| p.MaxMhz).max().unwrap_or(0);
    ((sum / live.len() as u64) as u32, max)
}

/// Turn an ACPI zone's instance name into something readable.
///
/// Names arrive as `\_TZ.CPUZ`: a path into the firmware's namespace. The last
/// four characters are an OEM-chosen tag, and only some of them are conventional
/// enough to translate. The rest are passed through as the firmware wrote them --
/// a made-up label on an unknown sensor is worse than a cryptic true one.
fn zone_label(instance: &str) -> Arc<str> {
    let tag = instance.rsplit(['.', '\\']).next().unwrap_or(instance);
    let known = match tag.to_ascii_uppercase().as_str() {
        "CPUZ" | "CPU" => Some("CPU"),
        "GFXZ" | "GPUZ" => Some("Graphics"),
        "PCHZ" => Some("Chipset"),
        "BATZ" => Some("Battery"),
        "CHGZ" => Some("Charger"),
        "SKINZ" | "SK1Z" | "SK2Z" => Some("Chassis"),
        "EXTZ" => Some("Exhaust"),
        "AMBZ" => Some("Ambient"),
        "WLNZ" | "WWANZ" => Some("Wireless"),
        "SSDZ" | "NVMZ" => Some("Storage"),
        _ => None,
    };
    match known {
        Some(name) if tag.to_ascii_uppercase().starts_with("SK") => {
            // Two skin sensors are common, and "Chassis" twice tells a reader
            // nothing about which is which.
            Arc::from(format!("{name} {}", &tag[2..3]).as_str())
        }
        Some(name) => Arc::from(name),
        None => Arc::from(tag),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::snapshot::vendor;

    #[test]
    fn names_the_zones_it_knows_and_passes_the_rest_through() {
        assert_eq!(&*zone_label(r"\_TZ.CPUZ"), "CPU");
        assert_eq!(&*zone_label(r"\_TZ.GFXZ"), "Graphics");
        assert_eq!(&*zone_label(r"\_TZ.SK1Z"), "Chassis 1");
        assert_eq!(&*zone_label(r"\_TZ.SK2Z"), "Chassis 2");
        // Not a convention anyone documents: shown as the firmware named it.
        assert_eq!(&*zone_label(r"\_TZ.MSHZ"), "MSHZ");
        assert_eq!(&*zone_label("BAGZ"), "BAGZ");
    }

    /// A note only for a library that is there and said nothing.
    ///
    /// `kernel32.dll` stands in for an installed vendor library, since it is the one
    /// DLL guaranteed to be loadable on any Windows: what is being tested is which
    /// case produces a line, not who answered.
    ///
    /// There used to be a third case here -- installed *and* answered -- and it is
    /// gone because it can no longer be reached: `silent` is called from the one arm
    /// of the registry loop that had no backend to show for the library, so "it
    /// answered" is not a state this function can be asked about.
    #[test]
    fn reports_a_library_that_is_installed_and_mute() {
        let mut notes = Vec::new();
        // Not installed at all: the ordinary case for two vendors out of three.
        silent(&mut notes, vendor::AMD, "no-such-vendor-lib.dll", "ADL");
        assert!(notes.is_empty(), "an absent library is not a fault");

        // Installed and mute: the case worth a line.
        silent(&mut notes, vendor::AMD, "kernel32.dll", "ADL");
        assert_eq!(notes.len(), 1);
        assert_eq!(
            notes[0].0,
            vendor::AMD,
            "tagged with the vendor it is about"
        );
        assert!(notes[0].1.contains("kernel32.dll") && notes[0].1.contains("ADL"));
    }

    /// Kelvin to Celsius, at the two ends of the plausible range. Reading Kelvin
    /// as Celsius is the mistake this counter invites.
    #[test]
    fn reads_the_counter_as_kelvin() {
        let c = |k: f64| (k - 273.15) as f32;
        assert!((c(328.0) - 54.85).abs() < 0.01, "a warm CPU is in the 50s");
        assert!((c(273.15) - 0.0).abs() < 0.01);
    }

    /// Read the machine this is running on, and check the readings are of the
    /// right *kind* -- not against fixed numbers, which would only pass on one
    /// machine. Run with `--nocapture` to see what it found.
    #[test]
    fn reads_this_machine_without_asking_for_rights() {
        let mut probe = SensorProbe::new();
        let mut out = Sensors::default();
        // Twice: a multi-instance counter yields nothing until its second
        // collection, and a probe that only ever works on the second tick is a
        // bug worth catching here rather than on screen.
        probe.sample(&mut out);
        probe.sample(&mut out);

        for z in &out.zones {
            println!("zone {:>10}  {:.1} C", &*z.label, z.celsius);
            assert!(
                (-40.0..150.0).contains(&z.celsius),
                "{} reads {} C, which is not a temperature",
                z.label,
                z.celsius
            );
        }
        for d in &out.gpus {
            println!(
                "gpu  [{}] vendor={:#06x}/{} {}  temp={:?} fan={:?}/{:?} core={:?} mem={:?}                  power={:?} util={:?} vram={}/{} MiB",
                d.source,
                d.vendor,
                d.ordinal,
                &*d.name,
                d.temp_c,
                d.fan_pct,
                d.fan_rpm,
                d.core_mhz,
                d.mem_mhz,
                d.power_w,
                d.util_pct,
                d.vram_used / (1024 * 1024),
                d.vram_total / (1024 * 1024)
            );
            if let Some(t) = d.temp_c {
                assert!((0.0..130.0).contains(&t), "GPU at {t} C");
            }
            if let Some(f) = d.fan_pct {
                assert!((0.0..=100.0).contains(&f), "fan at {f}%");
            }
        }
        println!(
            "cpu  clock {} / {} MHz  temp {:?} C  power {:?} W",
            out.cpu_mhz, out.cpu_max_mhz, out.cpu_temp_c, out.cpu_power_w
        );
        if let Some(w) = out.cpu_power_w {
            assert!(
                (0.05..1000.0).contains(&w),
                "{w} W is not a processor's power draw"
            );
        }
        if let Some(c) = out.cpu_temp_c {
            assert!((0.0..130.0).contains(&c), "CPU at {c} C");
        }
        if out.cpu_max_mhz > 0 {
            assert!(out.cpu_mhz > 0, "a nominal clock but no current one");
        }
    }
}
