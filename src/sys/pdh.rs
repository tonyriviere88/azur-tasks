//! The little of PDH this app needs: a query, and multi-instance counters read
//! into `(instance, value)` pairs.
//!
//! Counters are added by their *English* path even on a localized Windows.
//! `PdhAddCounterW` takes the display name in the machine's own language --
//! `\Thermal Zone Information(*)\Temperature` does not exist on a French install
//! -- while `PdhAddEnglishCounterW` resolves the same counter everywhere.

use super::util::{from_wide_nul, wide};

use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

const PDH_CSTATUS_VALID_DATA: u32 = 0;
const PDH_MORE_DATA: u32 = 0x8000_07D2;

/// An open PDH query. Closes itself.
pub struct Query(PDH_HQUERY);

impl Query {
    pub fn open() -> Option<Self> {
        let mut query: PDH_HQUERY = std::ptr::null_mut();
        (unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } == 0).then_some(Self(query))
    }

    pub fn handle(&self) -> PDH_HQUERY {
        self.0
    }

    /// Take a reading of every counter in the query. Returns false when the
    /// collection failed, which for a rate counter also happens on the first one.
    pub fn collect(&self) -> bool {
        unsafe { PdhCollectQueryData(self.0) == 0 }
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.0) };
    }
}

pub struct Counter {
    handle: PDH_HCOUNTER,
    buf: Vec<u8>,
}

impl Counter {
    pub fn add(query: &Query, path: &str) -> Option<Self> {
        let wpath = wide(path);
        let mut handle: PDH_HCOUNTER = std::ptr::null_mut();
        let rc = unsafe { PdhAddEnglishCounterW(query.handle(), wpath.as_ptr(), 0, &mut handle) };
        (rc == 0).then_some(Self {
            handle,
            buf: vec![0u8; 16 * 1024],
        })
    }

    /// Bytes held by the counter-array buffer. PDH sizes these from the instance
    /// count, so for a per-process counter set this tracks machine load.
    #[cfg(feature = "memtrace")]
    pub fn buf_bytes(&self) -> usize {
        self.buf.capacity()
    }

    /// Read the counter's instances as `(instance_name, value)` pairs.
    pub fn read(&mut self, out: &mut Vec<(String, f64)>) {
        out.clear();
        for _ in 0..3 {
            let mut size = self.buf.len() as u32;
            let mut count = 0u32;
            let rc = unsafe {
                PdhGetFormattedCounterArrayW(
                    self.handle,
                    PDH_FMT_DOUBLE,
                    &mut size,
                    &mut count,
                    self.buf.as_mut_ptr().cast(),
                )
            };

            if rc == PDH_MORE_DATA {
                self.buf.resize((size as usize).max(self.buf.len() * 2), 0);
                continue;
            }
            if rc != 0 {
                return;
            }

            let items = self.buf.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            for i in 0..count as usize {
                let item: PDH_FMT_COUNTERVALUE_ITEM_W =
                    unsafe { std::ptr::read_unaligned(items.add(i)) };
                if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA || item.szName.is_null() {
                    continue;
                }
                // szName points into `self.buf`, after the item array.
                let name = unsafe { read_pwstr(item.szName) };
                // SAFETY: CStatus == VALID_DATA means the union holds a double.
                let value = unsafe { item.FmtValue.Anonymous.doubleValue };
                out.push((name, value));
            }
            return;
        }
    }
}

unsafe fn read_pwstr(p: *const u16) -> String {
    let mut len = 0usize;
    // Instance names are short; the bound just prevents a runaway on a malformed
    // buffer.
    while len < 512 && *p.add(len) != 0 {
        len += 1;
    }
    from_wide_nul(std::slice::from_raw_parts(p, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which of the counter sets this app can use are published by this machine.
    ///
    /// A probe, not an assertion: counter sets come from device drivers and optional
    /// Windows features, so what is present is a property of the machine rather than
    /// of the code. Run it when a sensor is unexpectedly missing -- it separates "the
    /// app is not reading it" from "this machine does not publish it".
    #[test]
    #[ignore = "reports what this machine publishes; run with --nocapture"]
    fn reports_which_counter_sets_this_machine_has() {
        let q = Query::open().expect("PDH opens for any user");
        for path in [
            r"\Thermal Zone Information(*)\Temperature",
            r"\Energy Meter(*)\Power",
            r"\GPU Engine(*)\Utilization Percentage",
            r"\GPU Adapter Memory(*)\Dedicated Usage",
        ] {
            let Some(mut c) = Counter::add(&q, path) else {
                println!("{path:<52} absent");
                continue;
            };
            // Twice, a second apart: a multi-instance counter yields nothing until
            // its second collection, and a rate needs two samples to be a rate.
            q.collect();
            std::thread::sleep(std::time::Duration::from_millis(1100));
            q.collect();
            let mut items = Vec::new();
            c.read(&mut items);
            println!("{path:<52} {} instance(s)", items.len());
            // Just the first few: a per-process set has hundreds.
            for (name, v) in items.iter().take(6) {
                println!("        {name:<40} {v:.1}");
            }
        }
    }
}
