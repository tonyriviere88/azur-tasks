//! The one interface a graphics vendor's library has to offer, and the registry of
//! the libraries that offer it.
//!
//! Three exist — NVML for NVIDIA, Level Zero Sysman for Intel, ADL for AMD — and
//! they have nothing whatever in common but the two questions below: how many
//! adapters this library speaks for, and what one of those adapters currently reads.
//! There is no vendor-neutral way to ask. Each is a `LoadLibrary` and a table of
//! function pointers resolved at run time, absent without that vendor's driver, and
//! each spells every sensor differently underneath.
//!
//! What the trait buys is the **list**. [`super::sensors`] used to name all three
//! vendors and repeat a block per vendor in three places — loading them, noting the
//! ones installed and mute, and reading them — so a fourth vendor meant finding
//! those three sites. Now it walks [`BACKENDS`] and names none of them, and a fourth
//! vendor is one row here plus one `impl GpuBackend`.
//!
//! ## Order is part of the contract
//!
//! [`BACKENDS`] is walked in order and each backend's readings are appended in the
//! order it enumerates them, so [`Sensors::gpus`](super::snapshot::Sensors::gpus)
//! comes out in a stable order tick after tick. That is what lets an adapter be
//! pinned to its readings by `(vendor, ordinal)` rather than by name — see
//! [`GpuStats::sensors`](super::snapshot::GpuStats::sensors). Reordering this array
//! would silently re-pair every reading on a two-card machine.

use super::adl::Adl;
use super::level_zero::LevelZero;
use super::nvml::Nvml;
use super::snapshot::{vendor, GpuSensors};

/// One vendor's interface to its own adapters.
pub trait GpuBackend {
    /// How many adapters this library can speak for. Settled when it loaded.
    fn device_count(&self) -> u32;

    /// One adapter's readings, or `None` when that adapter could not be read.
    ///
    /// `&mut self` for Level Zero's sake alone: its Sysman handles are acquired on
    /// first use and cached behind them. The other two need nothing mutable and
    /// simply do not use it — which is the right trade, because the alternative is
    /// interior mutability in a place where nothing is shared.
    fn read(&mut self, index: u32) -> Option<GpuSensors>;
}

/// One row of the registry: a PCI vendor, the library that speaks for it, what to
/// call that library when reporting on it, and how to bind it.
pub struct Backend {
    /// The PCI vendor this library speaks for. What an adapter is matched against —
    /// never the product name, which turns on how a driver spells itself.
    pub vendor: u32,
    /// The file to look for when deciding whether silence means "no such card" or
    /// "the interface is here and mute".
    pub dll: &'static str,
    /// The interface's name, for the note a mute library gets. A page of
    /// vendor-specific numbers should say whose they are.
    pub api: &'static str,
    /// Bind it, or `None` when it is not on this machine.
    ///
    /// A function pointer rather than a generic, because the three concrete types
    /// have nothing in common and the registry has to be one array.
    pub load: fn() -> Option<Box<dyn GpuBackend>>,
}

/// Every vendor library this build knows how to ask, in the order they are read.
///
/// See the note on ordering in the module documentation before changing this.
pub const BACKENDS: [Backend; 3] = [
    Backend {
        vendor: vendor::NVIDIA,
        dll: "nvml.dll",
        api: "NVML",
        load: load_nvml,
    },
    Backend {
        vendor: vendor::INTEL,
        dll: "ze_loader.dll",
        api: "Level Zero",
        load: load_level_zero,
    },
    Backend {
        vendor: vendor::AMD,
        dll: "atiadlxx.dll",
        api: "ADL",
        load: load_adl,
    },
];

// The three loaders. Each is one line, and exists because a `const` array needs
// function *items* and `Nvml::load` has the wrong return type to be one.
fn load_nvml() -> Option<Box<dyn GpuBackend>> {
    Nvml::load().map(|b| Box::new(b) as Box<dyn GpuBackend>)
}

fn load_level_zero() -> Option<Box<dyn GpuBackend>> {
    LevelZero::load().map(|b| Box::new(b) as Box<dyn GpuBackend>)
}

fn load_adl() -> Option<Box<dyn GpuBackend>> {
    Adl::load().map(|b| Box::new(b) as Box<dyn GpuBackend>)
}
