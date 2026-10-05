//! Physical-disk counters via `IOCTL_DISK_PERFORMANCE`.
//!
//! This is the same source Task Manager uses. Opening `\\.\PhysicalDriveN` with
//! zero desired access needs no elevation -- we only issue informational IOCTLs,
//! never a read or write.

use super::snapshot::DiskStats;
use super::util::{wide, Handle};
use std::sync::Arc;

use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

// CTL_CODE(IOCTL_DISK_BASE=0x07, 0x0008, METHOD_BUFFERED, FILE_ANY_ACCESS)
const IOCTL_DISK_PERFORMANCE: u32 = 0x0007_0020;
// CTL_CODE(IOCTL_DISK_BASE=0x07, 0x0028, METHOD_BUFFERED, FILE_ANY_ACCESS).
// The obvious IOCTL_DISK_GET_LENGTH_INFO requires FILE_READ_ACCESS, which we
// deliberately do not hold; the GEOMETRY_EX variant is FILE_ANY_ACCESS and
// reports the same total size.
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
// CTL_CODE(IOCTL_STORAGE_BASE=0x2d, 0x0500, METHOD_BUFFERED, FILE_ANY_ACCESS)
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DiskPerformance {
    bytes_read: i64,
    bytes_written: i64,
    read_time: i64,
    write_time: i64,
    idle_time: i64,
    read_count: u32,
    write_count: u32,
    queue_depth: u32,
    split_count: u32,
    query_time: i64,
    storage_device_number: u32,
    storage_manager_name: [u16; 8],
}

#[repr(C)]
struct StoragePropertyQuery {
    property_id: u32,
    query_type: u32,
    additional: [u8; 1],
}

#[repr(C)]
#[derive(Default)]
struct StorageDeviceDescriptor {
    version: u32,
    size: u32,
    device_type: u8,
    device_type_modifier: u8,
    removable_media: u8,
    command_queueing: u8,
    vendor_id_offset: u32,
    product_id_offset: u32,
    product_revision_offset: u32,
    serial_number_offset: u32,
    bus_type: u32,
    raw_properties_length: u32,
    raw_device_properties: [u8; 1],
}

/// One tracked physical drive.
struct Drive {
    handle: Handle,
    index: u32,
    label: Arc<str>,
    capacity: u64,
    prev: Option<DiskPerformance>,
}

pub struct DiskProbe {
    drives: Vec<Drive>,
}

impl DiskProbe {
    /// Enumerate physical drives once. A drive appearing later needs a restart
    /// to show up, which matches how rarely that happens in practice.
    pub fn new() -> Self {
        let mut drives = Vec::new();
        // 32 is well past any realistic desktop/workstation drive count.
        for index in 0..32u32 {
            let path = wide(&format!(r"\\.\PhysicalDrive{index}"));
            // Zero desired access: enough for informational IOCTLs, and works
            // unelevated. Requesting GENERIC_READ here would need admin.
            let h = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            let Some(handle) = Handle::new(h) else {
                // Gaps are possible (a removed drive leaves a hole), so keep
                // probing rather than stopping at the first miss.
                continue;
            };

            // Confirm the perf counters actually answer before we keep it.
            let mut perf = DiskPerformance::default();
            let mut ret = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    handle.raw(),
                    IOCTL_DISK_PERFORMANCE,
                    std::ptr::null(),
                    0,
                    (&mut perf as *mut DiskPerformance).cast(),
                    std::mem::size_of::<DiskPerformance>() as u32,
                    &mut ret,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                continue;
            }

            let model = query_model(&handle);
            let capacity = query_capacity(&handle);
            let label: Arc<str> = match model {
                Some(m) if !m.is_empty() => format!("Disk {index} ({m})").into(),
                _ => format!("Disk {index}").into(),
            };

            drives.push(Drive {
                handle,
                index,
                label,
                capacity,
                prev: None,
            });
        }
        Self { drives }
    }

    /// Sample every drive, appending one `DiskStats` per drive to `out`.
    pub fn sample(&mut self, out: &mut Vec<DiskStats>) {
        for d in &mut self.drives {
            let mut perf = DiskPerformance::default();
            let mut ret = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    d.handle.raw(),
                    IOCTL_DISK_PERFORMANCE,
                    std::ptr::null(),
                    0,
                    (&mut perf as *mut DiskPerformance).cast(),
                    std::mem::size_of::<DiskPerformance>() as u32,
                    &mut ret,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                continue;
            }

            let mut stats = DiskStats {
                name: d.label.clone(),
                capacity: d.capacity,
                ..Default::default()
            };

            if let Some(p) = d.prev {
                // QueryTime is the counter's own clock, so active-time math is
                // immune to jitter in our sampling interval.
                let span = (perf.query_time - p.query_time) as f64;
                if span > 0.0 {
                    let idle = (perf.idle_time - p.idle_time).max(0) as f64;
                    stats.active_pct =
                        (((span - idle) / span * 100.0) as f32).clamp(0.0, 100.0);

                    let secs = span / 10_000_000.0;
                    if secs > 0.0 {
                        stats.read_bps =
                            ((perf.bytes_read - p.bytes_read).max(0) as f64 / secs) as f32;
                        stats.write_bps =
                            ((perf.bytes_written - p.bytes_written).max(0) as f64 / secs) as f32;
                    }
                }

                let ops = (perf.read_count.wrapping_sub(p.read_count)) as u64
                    + (perf.write_count.wrapping_sub(p.write_count)) as u64;
                if ops > 0 {
                    let busy = (perf.read_time - p.read_time).max(0) as f64
                        + (perf.write_time - p.write_time).max(0) as f64;
                    // 100 ns ticks -> ms
                    stats.avg_response_ms = (busy / ops as f64 / 10_000.0) as f32;
                }
            }

            d.prev = Some(perf);
            let _ = d.index;
            out.push(stats);
        }
    }
}

fn query_model(h: &Handle) -> Option<String> {
    let query = StoragePropertyQuery {
        property_id: 0, // StorageDeviceProperty
        query_type: 0,  // PropertyStandardQuery
        additional: [0],
    };
    let mut buf = vec![0u8; 1024];
    let mut ret = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            h.raw(),
            IOCTL_STORAGE_QUERY_PROPERTY,
            (&query as *const StoragePropertyQuery).cast(),
            std::mem::size_of::<StoragePropertyQuery>() as u32,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut ret,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || (ret as usize) < std::mem::size_of::<StorageDeviceDescriptor>() {
        return None;
    }

    let desc: StorageDeviceDescriptor = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
    // Offsets are byte offsets into this same buffer; 0 means "not present".
    let read_at = |off: u32| -> Option<String> {
        if off == 0 || off as usize >= ret as usize {
            return None;
        }
        let bytes = &buf[off as usize..ret as usize];
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        let s = String::from_utf8_lossy(&bytes[..end]).trim().to_owned();
        (!s.is_empty()).then_some(s)
    };

    let vendor = read_at(desc.vendor_id_offset);
    let product = read_at(desc.product_id_offset);
    match (vendor, product) {
        (Some(v), Some(p)) if !p.starts_with(&v) => Some(format!("{v} {p}")),
        (_, Some(p)) => Some(p),
        (Some(v), None) => Some(v),
        _ => None,
    }
}

fn query_capacity(h: &Handle) -> u64 {
    // DISK_GEOMETRY_EX: DISK_GEOMETRY (24 bytes on x64) then DiskSize (i64).
    //   DISK_GEOMETRY { Cylinders: i64, MediaType: u32, TracksPerCylinder: u32,
    //                   SectorsPerTrack: u32, BytesPerSector: u32 }
    #[repr(C)]
    #[derive(Default)]
    struct DiskGeometryEx {
        cylinders: i64,
        media_type: u32,
        tracks_per_cylinder: u32,
        sectors_per_track: u32,
        bytes_per_sector: u32,
        disk_size: i64,
        // `Data[1]` in the SDK; the partition info that follows is unused here.
        _data: [u8; 8],
    }

    let mut geo = DiskGeometryEx::default();
    let mut ret = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            h.raw(),
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            std::ptr::null(),
            0,
            (&mut geo as *mut DiskGeometryEx).cast(),
            std::mem::size_of::<DiskGeometryEx>() as u32,
            &mut ret,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || geo.disk_size <= 0 {
        // Unknown capacity is displayed as a blank, not as zero bytes.
        return 0;
    }
    geo.disk_size as u64
}
