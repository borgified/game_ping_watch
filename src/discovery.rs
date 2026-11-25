use std::collections::{BTreeSet, HashMap};
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use etherparse::{NetSlice, SlicedPacket, TransportSlice};

use crate::udp_monitor::{open_packet_socket, unwrap_ip_payload};
use crate::{is_pub_v4, STOP};

/// Output of the initial discovery capture phase handed off to monitoring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveryResult {
    pub target_ip: Ipv4Addr,
    pub udp_ports: BTreeSet<u16>,
    pub total_packets: u64,
    pub total_bytes: u64,
}

/// Pure packet aggregator for candidate server ranking and UDP port association.
#[derive(Debug, Default)]
pub(crate) struct DiscoveryTracker {
    counts: HashMap<Ipv4Addr, (u64, u64, u16)>,
    udp_ports_by_ip: HashMap<Ipv4Addr, BTreeSet<u16>>,
}

impl DiscoveryTracker {
    /// Record one parsed IPv4 TCP/UDP packet into candidate counts and per-IP UDP port sets.
    pub(crate) fn record_packet(
        &mut self,
        src: IpAddr,
        dst: IpAddr,
        is_udp: bool,
        sp: u16,
        dp: u16,
        frame_len: usize,
        port_filter: Option<u16>,
    ) {
        let is_excluded = matches!(sp, 53 | 1900 | 5353) || matches!(dp, 53 | 1900 | 5353);
        let explicitly_requested = port_filter.is_some_and(|p| sp == p || dp == p);
        if is_excluded && !explicitly_requested {
            return;
        }

        let (rip, rport) = if let Some(v) = is_pub_v4(src) {
            (v, sp)
        } else if let Some(v) = is_pub_v4(dst) {
            (v, dp)
        } else {
            return;
        };

        if is_udp {
            self.udp_ports_by_ip.entry(rip).or_default().insert(rport);
        }

        if let Some(p) = port_filter {
            if sp != p && dp != p {
                return;
            }
        }

        let entry = self.counts.entry(rip).or_insert((0, 0, rport));
        entry.0 += 1;
        entry.1 += frame_len as u64;
        entry.2 = rport;
    }

    /// Return the top `n` public IPv4 candidates sorted by descending byte volume.
    pub(crate) fn top_candidates(&self, n: usize) -> Vec<(Ipv4Addr, u64, u64, u16)> {
        let mut top: Vec<_> = self
            .counts
            .iter()
            .map(|(&ip, &(pkts, bytes, last_port))| (ip, pkts, bytes, last_port))
            .collect();
        top.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        top.truncate(n);
        top
    }

    /// Select the top-ranked public IPv4 candidate and all UDP ports associated with it.
    pub(crate) fn into_result(mut self) -> Option<DiscoveryResult> {
        let (target_ip, total_packets, total_bytes, _) =
            self.top_candidates(1).into_iter().next()?;
        let udp_ports = self.udp_ports_by_ip.remove(&target_ip).unwrap_or_default();
        Some(DiscoveryResult {
            target_ip,
            udp_ports,
            total_packets,
            total_bytes,
        })
    }
}

/// Format an optional sorted UDP port set for console display (`none` when empty).
pub(crate) fn format_ports_list(ports: &BTreeSet<u16>, empty_label: &str) -> String {
    if ports.is_empty() {
        empty_label.to_string()
    } else {
        ports
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Capture live traffic for `secs` seconds to discover the top public server IP and its UDP ports.
pub(crate) fn discover_ip(
    secs: u64,
    port: Option<u16>,
    iface: Option<&str>,
) -> io::Result<Option<DiscoveryResult>> {
    let fd = open_packet_socket(iface, None, 200_000)?;
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut frame_buf = vec![0u8; 65536];
    let mut tracker = DiscoveryTracker::default();

    while !STOP.load(Ordering::SeqCst) && Instant::now() < deadline {
        let n = unsafe {
            libc::recvfrom(
                fd.0,
                frame_buf.as_mut_ptr() as *mut libc::c_void,
                frame_buf.len(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if n <= 0 {
            let e = io::Error::last_os_error();
            match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted => continue,
                _ => return Err(e),
            }
        }
        let f = &frame_buf[..n as usize];
        let Some(ip_slice) = unwrap_ip_payload(f) else {
            continue;
        };
        let Ok(sliced) = SlicedPacket::from_ip(ip_slice) else {
            continue;
        };
        let (src, dst) = match &sliced.net {
            Some(NetSlice::Ipv4(v4)) => (
                IpAddr::V4(v4.header().source_addr()),
                IpAddr::V4(v4.header().destination_addr()),
            ),
            _ => continue,
        };
        let (is_udp, sp, dp) = match sliced.transport {
            Some(TransportSlice::Udp(u)) => (true, u.source_port(), u.destination_port()),
            Some(TransportSlice::Tcp(t)) => (false, t.source_port(), t.destination_port()),
            _ => continue,
        };

        tracker.record_packet(src, dst, is_udp, sp, dp, f.len(), port);
    }

    let top = tracker.top_candidates(5);
    eprintln!("\nTop IPv4 candidates:");
    for (ip, pkts, bytes, p) in &top {
        eprintln!("  {:<15}:{:<5} pkts={:<6} bytes={}", ip, p, pkts, bytes);
    }

    let result = tracker.into_result();
    if let Some(ref res) = result {
        eprintln!(
            "Selected target: {} (associated UDP ports: {})",
            res.target_ip,
            format_ports_list(&res.udp_ports, "none")
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_multiple_udp_ports_for_selected_target() {
        let mut tracker = DiscoveryTracker::default();
        let local = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
        let srv1 = IpAddr::V4(Ipv4Addr::new(92, 223, 1, 10));
        let srv2 = IpAddr::V4(Ipv4Addr::new(92, 223, 1, 20));

        tracker.record_packet(local, srv1, true, 50000, 32862, 400, Some(32862));
        tracker.record_packet(srv1, local, true, 32863, 50001, 200, Some(32862));
        tracker.record_packet(local, srv2, true, 50002, 40000, 150, Some(32862));

        let res = tracker.into_result().expect("should find top server");
        assert_eq!(res.target_ip, Ipv4Addr::new(92, 223, 1, 10));
        assert_eq!(res.udp_ports, BTreeSet::from([32862, 32863]));
        assert_eq!(res.total_packets, 1);
        assert_eq!(res.total_bytes, 400);
    }

    #[test]
    fn excludes_background_ports_unless_explicitly_filtered() {
        let local = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let pub_ip = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));

        let mut default_tracker = DiscoveryTracker::default();
        default_tracker.record_packet(local, pub_ip, true, 44444, 53, 100, None);
        default_tracker.record_packet(local, pub_ip, true, 44445, 5353, 100, None);
        assert!(default_tracker.into_result().is_none());

        let mut explicit_tracker = DiscoveryTracker::default();
        explicit_tracker.record_packet(local, pub_ip, true, 44444, 53, 100, Some(53));
        let res = explicit_tracker
            .into_result()
            .expect("explicit port 53 should be tracked");
        assert_eq!(res.udp_ports, BTreeSet::from([53]));
    }

    #[test]
    fn handles_tcp_only_discovery_with_empty_udp_ports() {
        let mut tracker = DiscoveryTracker::default();
        let local = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
        let tcp_srv = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));

        tracker.record_packet(local, tcp_srv, false, 51000, 443, 1200, None);
        let res = tracker.into_result().expect("should find TCP server");
        assert_eq!(res.target_ip, Ipv4Addr::new(1, 1, 1, 1));
        assert!(res.udp_ports.is_empty());
    }
}
