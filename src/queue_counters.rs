use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Cumulative per-interface drop and error counters from Linux sysfs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NicDropSnapshot {
    pub rx_dropped: u64,
    pub tx_dropped: u64,
    pub rx_missed_errors: u64,
    pub rx_fifo_errors: u64,
    pub tx_fifo_errors: u64,
}

impl NicDropSnapshot {
    fn metrics(&self) -> [(&'static str, u64); 5] {
        [
            ("rx_dropped", self.rx_dropped),
            ("tx_dropped", self.tx_dropped),
            ("rx_missed_errors", self.rx_missed_errors),
            ("rx_fifo_errors", self.rx_fifo_errors),
            ("tx_fifo_errors", self.tx_fifo_errors),
        ]
    }
}

/// System-wide UDP error and buffer drop counters from `/proc/net/snmp`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HostUdpSnapshot {
    pub rcvbuf_errors: u64,
    pub sndbuf_errors: u64,
    pub in_errors: u64,
    pub in_csum_errors: u64,
}

impl HostUdpSnapshot {
    fn metrics(&self) -> [(&'static str, u64); 4] {
        [
            ("RcvbufErrors", self.rcvbuf_errors),
            ("SndbufErrors", self.sndbuf_errors),
            ("InErrors", self.in_errors),
            ("InCsumErrors", self.in_csum_errors),
        ]
    }
}

/// Non-zero local queue drop delta observed during polling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueueDropEvent {
    pub scope: String,
    pub metric: &'static str,
    pub delta: u64,
    pub category: DropCategory,
}

/// Classification of a local queue drop counter for session totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropCategory {
    Nic,
    HostUdp,
    Capture,
}

/// Tracks baseline and cumulative deltas for NIC, host UDP, and AF_PACKET ring drops.
#[derive(Debug, Clone, Default)]
pub(crate) struct LocalQueueCounters {
    pub nic_drops: BTreeMap<String, NicDropSnapshot>,
    pub host_udp: HostUdpSnapshot,
    pub capture_drops: u64,
}

impl LocalQueueCounters {
    /// Capture an initial baseline snapshot before the monitoring loop begins.
    pub(crate) fn baseline(iface: Option<&str>, packet_fd: Option<libc::c_int>) -> Self {
        if let Some(fd) = packet_fd {
            // Reset kernel AF_PACKET stats counter so pre-loop setup drops are excluded.
            let _ = read_capture_drops(fd);
        }
        Self {
            nic_drops: read_nic_snapshots(iface),
            host_udp: read_host_udp_snapshot().unwrap_or_default(),
            capture_drops: 0,
        }
    }

    /// Poll live Linux counters and return any non-zero drop deltas since the last poll.
    pub(crate) fn poll_deltas(
        &mut self,
        iface: Option<&str>,
        packet_fd: Option<libc::c_int>,
    ) -> Vec<QueueDropEvent> {
        let next_nic = read_nic_snapshots(iface);
        let next_host = read_host_udp_snapshot().unwrap_or_else(|| self.host_udp.clone());
        let cap_delta = packet_fd.map(read_capture_drops).unwrap_or(0);
        self.diff_snapshots(next_nic, next_host, cap_delta)
    }

    /// Apply new snapshots using saturating deltas and emit events for non-zero increments.
    pub(crate) fn diff_snapshots(
        &mut self,
        next_nic: BTreeMap<String, NicDropSnapshot>,
        next_host: HostUdpSnapshot,
        cap_delta: u64,
    ) -> Vec<QueueDropEvent> {
        let mut events = Vec::new();

        for (ifname, cur) in &next_nic {
            if let Some(prev) = self.nic_drops.get(ifname) {
                for ((metric, cur_v), (_, prev_v)) in cur.metrics().into_iter().zip(prev.metrics()) {
                    let delta = cur_v.saturating_sub(prev_v);
                    if delta > 0 {
                        events.push(QueueDropEvent {
                            scope: format!("nic:{ifname}"),
                            metric,
                            delta,
                            category: DropCategory::Nic,
                        });
                    }
                }
            }
        }
        self.nic_drops = next_nic;

        for ((metric, cur_v), (_, prev_v)) in
            next_host.metrics().into_iter().zip(self.host_udp.metrics())
        {
            let delta = cur_v.saturating_sub(prev_v);
            if delta > 0 {
                events.push(QueueDropEvent {
                    scope: "host:udp".to_string(),
                    metric,
                    delta,
                    category: DropCategory::HostUdp,
                });
            }
        }
        self.host_udp = next_host;

        if cap_delta > 0 {
            self.capture_drops = self.capture_drops.saturating_add(cap_delta);
            events.push(QueueDropEvent {
                scope: "capture:af_packet".to_string(),
                metric: "tp_drops",
                delta: cap_delta,
                category: DropCategory::Capture,
            });
        }

        events
    }
}

/// Parse `/proc/net/snmp` by matching the `Udp:` header column names against the `Udp:` values row.
pub(crate) fn parse_proc_net_snmp_udp(contents: &str) -> Option<HostUdpSnapshot> {
    let mut lines = contents.lines();
    while let Some(header_line) = lines.next() {
        let Some(header_rest) = header_line.strip_prefix("Udp:") else {
            continue;
        };
        let value_line = lines.next()?;
        let value_rest = value_line.strip_prefix("Udp:")?;
        let headers: Vec<&str> = header_rest.split_whitespace().collect();
        let values: Vec<&str> = value_rest.split_whitespace().collect();
        if headers.len() != values.len() {
            return None;
        }
        let mut snap = HostUdpSnapshot::default();
        for (k, v) in headers.iter().zip(values.iter()) {
            let Ok(num) = v.parse::<u64>() else { continue };
            match *k {
                "RcvbufErrors" => snap.rcvbuf_errors = num,
                "SndbufErrors" => snap.sndbuf_errors = num,
                "InErrors" => snap.in_errors = num,
                "InCsumErrors" => snap.in_csum_errors = num,
                _ => {}
            }
        }
        return Some(snap);
    }
    None
}

fn read_host_udp_snapshot() -> Option<HostUdpSnapshot> {
    let contents = fs::read_to_string("/proc/net/snmp").ok()?;
    parse_proc_net_snmp_udp(&contents)
}

fn read_nic_stat(stats_dir: &Path, name: &str) -> u64 {
    fs::read_to_string(stats_dir.join(name))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn read_nic_snapshot_dir(stats_dir: &Path) -> Option<NicDropSnapshot> {
    if !stats_dir.is_dir() {
        return None;
    }
    Some(NicDropSnapshot {
        rx_dropped: read_nic_stat(stats_dir, "rx_dropped"),
        tx_dropped: read_nic_stat(stats_dir, "tx_dropped"),
        rx_missed_errors: read_nic_stat(stats_dir, "rx_missed_errors"),
        rx_fifo_errors: read_nic_stat(stats_dir, "rx_fifo_errors"),
        tx_fifo_errors: read_nic_stat(stats_dir, "tx_fifo_errors"),
    })
}

/// Read sysfs interface drop counters for `iface` or all non-loopback interfaces when `None`.
pub(crate) fn read_nic_snapshots(iface: Option<&str>) -> BTreeMap<String, NicDropSnapshot> {
    let mut map = BTreeMap::new();
    let net_root = Path::new("/sys/class/net");
    if let Some(name) = iface {
        let stats_dir = net_root.join(name).join("statistics");
        if let Some(snap) = read_nic_snapshot_dir(&stats_dir) {
            map.insert(name.to_string(), snap);
        }
        return map;
    }

    let Ok(entries) = fs::read_dir(net_root) else {
        return map;
    };
    let mut names = BTreeSet::new();
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name != "lo" {
                names.insert(name.to_string());
            }
        }
    }
    for name in names {
        let stats_dir = net_root.join(&name).join("statistics");
        if let Some(snap) = read_nic_snapshot_dir(&stats_dir) {
            map.insert(name, snap);
        }
    }
    map
}

/// Read and reset kernel `AF_PACKET` `PACKET_STATISTICS` `tp_drops` on `fd`.
pub(crate) fn read_capture_drops(fd: libc::c_int) -> u64 {
    let mut stats: libc::tpacket_stats = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::tpacket_stats>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_PACKET,
            libc::PACKET_STATISTICS,
            &mut stats as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 {
        stats.tp_drops as u64
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_net_snmp_udp_by_column_name() {
        let sample = "\
Ip: Forwarding DefaultTTL InReceives
Ip: 2 64 100
Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors
Udp: 82970 19 3 68146 12 4 2 2200 0
";
        let snap = parse_proc_net_snmp_udp(sample).expect("should parse Udp section");
        assert_eq!(snap.in_errors, 3);
        assert_eq!(snap.rcvbuf_errors, 12);
        assert_eq!(snap.sndbuf_errors, 4);
        assert_eq!(snap.in_csum_errors, 2);
    }

    #[test]
    fn computes_saturating_deltas_across_all_scopes() {
        let mut nic0 = BTreeMap::new();
        nic0.insert(
            "enp37s0".to_string(),
            NicDropSnapshot {
                rx_dropped: 10,
                tx_dropped: 2,
                ..Default::default()
            },
        );
        let mut counters = LocalQueueCounters {
            nic_drops: nic0,
            host_udp: HostUdpSnapshot {
                rcvbuf_errors: 5,
                ..Default::default()
            },
            capture_drops: 0,
        };

        let mut nic1 = BTreeMap::new();
        nic1.insert(
            "enp37s0".to_string(),
            NicDropSnapshot {
                rx_dropped: 14,
                tx_dropped: 1, // simulated reset must saturate to 0
                ..Default::default()
            },
        );
        let host1 = HostUdpSnapshot {
            rcvbuf_errors: 8,
            in_errors: 1,
            ..Default::default()
        };

        let events = counters.diff_snapshots(nic1, host1, 2);
        assert_eq!(events.len(), 4);
        assert!(events.contains(&QueueDropEvent {
            scope: "nic:enp37s0".to_string(),
            metric: "rx_dropped",
            delta: 4,
            category: DropCategory::Nic,
        }));
        assert!(events.contains(&QueueDropEvent {
            scope: "host:udp".to_string(),
            metric: "RcvbufErrors",
            delta: 3,
            category: DropCategory::HostUdp,
        }));
        assert!(events.contains(&QueueDropEvent {
            scope: "host:udp".to_string(),
            metric: "InErrors",
            delta: 1,
            category: DropCategory::HostUdp,
        }));
        assert!(events.contains(&QueueDropEvent {
            scope: "capture:af_packet".to_string(),
            metric: "tp_drops",
            delta: 2,
            category: DropCategory::Capture,
        }));
        assert_eq!(counters.capture_drops, 2);
    }
}
