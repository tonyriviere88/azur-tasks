//! Per-interface network throughput via `GetIfTable2`.

use super::snapshot::NetStats;
use super::util::from_wide_nul;
use rustc_hash::FxHashMap;
use std::sync::Arc;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2,
};

/// IF_TYPE_SOFTWARE_LOOPBACK
const IF_TYPE_LOOPBACK: u32 = 24;
/// IF_TYPE_TUNNEL
const IF_TYPE_TUNNEL: u32 = 131;
/// IfOperStatusUp
const IF_OPER_STATUS_UP: i32 = 1;

/// `MIB_IF_ROW2::InterfaceAndOperStatusFlags` bit 1 -- the interface is an NDIS
/// filter module layered over a real adapter, not an adapter itself.
const FLAG_FILTER_INTERFACE: u8 = 1 << 1;

#[derive(Clone, Copy)]
struct Prev {
    in_octets: u64,
    out_octets: u64,
}

pub struct NetProbe {
    /// Keyed by interface LUID, which is stable across enable/disable cycles
    /// (unlike the interface index).
    prev: FxHashMap<u64, Prev>,
    names: FxHashMap<u64, Arc<str>>,
}

impl NetProbe {
    pub fn new() -> Self {
        Self {
            prev: FxHashMap::default(),
            names: FxHashMap::default(),
        }
    }

    /// Sample all usable interfaces. `dt` is the elapsed seconds since the last
    /// call and is used to turn octet counters into rates.
    pub fn sample(&mut self, dt: f32, out: &mut Vec<NetStats>) {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if unsafe { GetIfTable2(&mut table) } != ERROR_SUCCESS || table.is_null() {
            return;
        }

        // Guarantee FreeMibTable runs even if a later step returns early.
        struct Guard(*mut MIB_IF_TABLE2);
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe { FreeMibTable(self.0.cast()) };
            }
        }
        let _guard = Guard(table);

        let count = unsafe { (*table).NumEntries } as usize;
        let rows = unsafe { std::ptr::addr_of!((*table).Table).cast::<MIB_IF_ROW2>() };

        for i in 0..count {
            let row: MIB_IF_ROW2 = unsafe { std::ptr::read_unaligned(rows.add(i)) };

            // Skip loopback and tunnel pseudo-interfaces: they double-count
            // traffic and are not what "Ethernet"/"Wi-Fi" means to a user.
            if row.Type == IF_TYPE_LOOPBACK || row.Type == IF_TYPE_TUNNEL {
                continue;
            }

            // Every NDIS lightweight filter bound to an adapter (QoS scheduler,
            // WFP MAC layer, Npcap, ...) shows up here as its own interface
            // reporting the *same* octets as the adapter underneath it. Without
            // this a single NIC appears six times.
            if row.InterfaceAndOperStatusFlags._bitfield & FLAG_FILTER_INTERFACE != 0 {
                continue;
            }

            let up = row.OperStatus == IF_OPER_STATUS_UP;

            // SAFETY: NET_LUID_LH is a union of a u64 `Value` and a bitfield.
            let luid: u64 = unsafe { row.InterfaceLuid.Value };

            let name = self
                .names
                .entry(luid)
                .or_insert_with(|| {
                    let alias = from_wide_nul(&row.Alias);
                    let desc = from_wide_nul(&row.Description);
                    let pick = if !alias.is_empty() { alias } else { desc };
                    Arc::<str>::from(if pick.is_empty() {
                        "Network adapter".to_owned()
                    } else {
                        pick
                    })
                })
                .clone();

            let mut stats = NetStats {
                name,
                link_speed: row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed),
                up,
                ..Default::default()
            };

            let cur = Prev {
                in_octets: row.InOctets,
                out_octets: row.OutOctets,
            };
            if let Some(p) = self.prev.get(&luid) {
                if dt > 0.0 {
                    // saturating_sub guards against a counter reset when an
                    // adapter is disabled and re-enabled.
                    stats.recv_bps = cur.in_octets.saturating_sub(p.in_octets) as f32 / dt;
                    stats.send_bps = cur.out_octets.saturating_sub(p.out_octets) as f32 / dt;
                }
            }
            self.prev.insert(luid, cur);

            // Keep interfaces that are actually usable or have moved bytes.
            // "Up with a zero link speed" describes idle RAS/WAN miniports,
            // which Task Manager does not list either.
            let usable = up && stats.link_speed > 0;
            if usable || cur.in_octets > 0 || cur.out_octets > 0 {
                out.push(stats);
            }
        }

        // Show the live adapters first, busiest first within each group.
        out.sort_by(|a, b| {
            b.up.cmp(&a.up).then_with(|| {
                (b.recv_bps + b.send_bps)
                    .total_cmp(&(a.recv_bps + a.send_bps))
                    .then_with(|| a.name.cmp(&b.name))
            })
        });
    }
}
