use std::collections::{BTreeSet, HashMap};
use std::ffi::CString;
use std::io::{self, ErrorKind};
use std::net::Ipv4Addr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use etherparse::{NetSlice, SlicedPacket, TransportSlice};

use crate::queue_counters::{DropCategory, LocalQueueCounters, QueueDropEvent};
use crate::{log_line, Fd, STOP};

const ETH_P_IP: u16 = 0x0800;
const ETH_P_8021Q: u16 = 0x8100;
const POLL_TICK_MS: u64 = 500;
const CAPTURE_RCVBUF_BYTES: usize = 4 * 1024 * 1024;

/// Traffic direction relative to the local host and target server IP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Direction::Inbound => "in",
            Direction::Outbound => "out",
        }
    }
}

/// Unique directional identifier for a monitored UDP packet stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct UdpFlowKey {
    pub ifindex: i32,
    pub local_port: u16,
    pub remote_port: u16,
    pub direction: Direction,
}

/// Lifecycle state for per-flow cadence baseline and anomaly heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlowPhase {
    WarmingUp { samples: u8, target: u8 },
    Active,
    Stalled { gap_reported: bool },
    Idle,
}

/// Per-flow cadence baseline and anomaly tracking state.
#[derive(Debug, Clone)]
pub(crate) struct UdpFlowState {
    pub phase: FlowPhase,
    pub packet_count: u64,
    pub last_seen: Duration,
    pub baseline_iat_ms: f64,
    pub pending_gap_ms: Option<f64>,
    pub compressed_streak: u32,
    pub burst_min_iat_ms: f64,
    pub in_burst_episode: bool,
    pub gap_events: u64,
    pub burst_events: u64,
}

impl UdpFlowState {
    fn new(now: Duration, warmup_target: u8) -> Self {
        Self {
            phase: FlowPhase::WarmingUp {
                samples: 0,
                target: warmup_target,
            },
            packet_count: 1,
            last_seen: now,
            baseline_iat_ms: 0.0,
            pending_gap_ms: None,
            compressed_streak: 0,
            burst_min_iat_ms: f64::INFINITY,
            in_burst_episode: false,
            gap_events: 0,
            burst_events: 0,
        }
    }
}

/// Tunable thresholds for flow-gap and heuristic burst/reordering detection.
#[derive(Debug, Clone)]
pub(crate) struct HeuristicConfig {
    pub warmup_samples: u8,
    pub re_warmup_samples: u8,
    pub idle_ms: f64,
    pub min_gap_ms: f64,
    pub gap_multiplier: f64,
    pub max_steady_baseline_ms: f64,
    pub min_compressed_ms: f64,
    pub compressed_ratio: f64,
    pub min_burst_baseline_ms: f64,
    pub post_stall_burst_packets: u32,
    pub unprovoked_burst_packets: u32,
}

impl Default for HeuristicConfig {
    fn default() -> Self {
        Self {
            warmup_samples: 10,
            re_warmup_samples: 5,
            idle_ms: 2000.0,
            min_gap_ms: 150.0,
            gap_multiplier: 4.0,
            max_steady_baseline_ms: 250.0,
            min_compressed_ms: 2.0,
            compressed_ratio: 0.25,
            min_burst_baseline_ms: 10.0,
            post_stall_burst_packets: 3,
            unprovoked_burst_packets: 4,
        }
    }
}

/// Real-time diagnostic events produced by the UDP monitor.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UdpAnomalyEvent {
    PortDiscovered {
        remote_port: u16,
        local_port: u16,
        direction: Direction,
    },
    FlowGap {
        remote_port: u16,
        local_port: u16,
        direction: Direction,
        gap_ms: f64,
        baseline_ms: f64,
    },
    HeuristicBurst {
        remote_port: u16,
        local_port: u16,
        direction: Direction,
        burst_packets: u32,
        preceding_gap_ms: f64,
        min_iat_ms: f64,
    },
}

/// End-of-session aggregated UDP monitoring statistics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UdpSummary {
    pub observed_ports: BTreeSet<u16>,
    pub total_udp_packets: u64,
    pub inbound_packets: u64,
    pub outbound_packets: u64,
    pub local_nic_drops: u64,
    pub local_host_udp_drops: u64,
    pub local_capture_drops: u64,
    pub flow_gap_events: u64,
    pub heuristic_burst_events: u64,
}

/// Pure directional UDP flow tracker and timing heuristic engine.
pub(crate) struct UdpFlowTracker {
    target_ip: Ipv4Addr,
    config: HeuristicConfig,
    observed_ports: BTreeSet<u16>,
    flows: HashMap<UdpFlowKey, UdpFlowState>,
    inbound_packets: u64,
    outbound_packets: u64,
    local_nic_drops: u64,
    local_host_udp_drops: u64,
    local_capture_drops: u64,
    flow_gap_events: u64,
    heuristic_burst_events: u64,
}

impl UdpFlowTracker {
    pub(crate) fn new(
        target_ip: Ipv4Addr,
        initial_ports: BTreeSet<u16>,
        config: HeuristicConfig,
    ) -> Self {
        Self {
            target_ip,
            config,
            observed_ports: initial_ports,
            flows: HashMap::new(),
            inbound_packets: 0,
            outbound_packets: 0,
            local_nic_drops: 0,
            local_host_udp_drops: 0,
            local_capture_drops: 0,
            flow_gap_events: 0,
            heuristic_burst_events: 0,
        }
    }

    /// Record confirmed local queue drop deltas into session totals.
    pub(crate) fn record_queue_drops(&mut self, events: &[QueueDropEvent]) {
        for ev in events {
            match ev.category {
                DropCategory::Nic => {
                    self.local_nic_drops = self.local_nic_drops.saturating_add(ev.delta);
                }
                DropCategory::HostUdp => {
                    self.local_host_udp_drops = self.local_host_udp_drops.saturating_add(ev.delta);
                }
                DropCategory::Capture => {
                    self.local_capture_drops = self.local_capture_drops.saturating_add(ev.delta);
                }
            }
        }
    }

    /// Process a single captured UDP packet and return any resulting anomaly events.
    pub(crate) fn on_packet(
        &mut self,
        now: Duration,
        ifindex: i32,
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
    ) -> Vec<UdpAnomalyEvent> {
        let (direction, local_port, remote_port) = if src_ip == self.target_ip {
            (Direction::Inbound, dst_port, src_port)
        } else if dst_ip == self.target_ip {
            (Direction::Outbound, src_port, dst_port)
        } else {
            return Vec::new();
        };

        let mut events = Vec::new();
        if self.observed_ports.insert(remote_port) {
            events.push(UdpAnomalyEvent::PortDiscovered {
                remote_port,
                local_port,
                direction,
            });
        }

        match direction {
            Direction::Inbound => self.inbound_packets += 1,
            Direction::Outbound => self.outbound_packets += 1,
        }

        let key = UdpFlowKey {
            ifindex,
            local_port,
            remote_port,
            direction,
        };

        let Some(state) = self.flows.get_mut(&key) else {
            self.flows
                .insert(key, UdpFlowState::new(now, self.config.warmup_samples));
            return events;
        };

        let iat_ms = now.saturating_sub(state.last_seen).as_secs_f64() * 1000.0;
        state.last_seen = now;
        state.packet_count += 1;

        if iat_ms >= self.config.idle_ms {
            state.phase = FlowPhase::WarmingUp {
                samples: 0,
                target: self.config.re_warmup_samples,
            };
            state.pending_gap_ms = None;
            state.compressed_streak = 0;
            state.burst_min_iat_ms = f64::INFINITY;
            state.in_burst_episode = false;
            return events;
        }

        match state.phase {
            FlowPhase::Idle => {
                state.phase = FlowPhase::WarmingUp {
                    samples: 0,
                    target: self.config.re_warmup_samples,
                };
                state.pending_gap_ms = None;
                state.compressed_streak = 0;
                state.burst_min_iat_ms = f64::INFINITY;
                state.in_burst_episode = false;
            }
            FlowPhase::WarmingUp { samples, target } => {
                let next_samples = samples.saturating_add(1);
                let n = next_samples as f64;
                state.baseline_iat_ms = if samples == 0 {
                    iat_ms
                } else {
                    state.baseline_iat_ms + (iat_ms - state.baseline_iat_ms) / n
                };
                if next_samples >= target {
                    state.phase = FlowPhase::Active;
                } else {
                    state.phase = FlowPhase::WarmingUp {
                        samples: next_samples,
                        target,
                    };
                }
            }
            FlowPhase::Active | FlowPhase::Stalled { .. } => {
                let gap_threshold_ms = (self.config.gap_multiplier * state.baseline_iat_ms)
                    .max(self.config.min_gap_ms);

                if matches!(state.phase, FlowPhase::Active)
                    && state.baseline_iat_ms <= self.config.max_steady_baseline_ms
                    && iat_ms >= gap_threshold_ms
                {
                    state.gap_events += 1;
                    self.flow_gap_events += 1;
                    state.pending_gap_ms = Some(iat_ms);
                    events.push(UdpAnomalyEvent::FlowGap {
                        remote_port,
                        local_port,
                        direction,
                        gap_ms: iat_ms,
                        baseline_ms: state.baseline_iat_ms,
                    });
                } else if let FlowPhase::Stalled { gap_reported } = state.phase {
                    if !gap_reported && state.baseline_iat_ms <= self.config.max_steady_baseline_ms
                    {
                        state.gap_events += 1;
                        self.flow_gap_events += 1;
                        events.push(UdpAnomalyEvent::FlowGap {
                            remote_port,
                            local_port,
                            direction,
                            gap_ms: iat_ms,
                            baseline_ms: state.baseline_iat_ms,
                        });
                    }
                    state.pending_gap_ms = Some(state.pending_gap_ms.unwrap_or(iat_ms).max(iat_ms));
                    state.phase = FlowPhase::Active;
                }

                let compressed_limit_ms = (self.config.compressed_ratio * state.baseline_iat_ms)
                    .max(self.config.min_compressed_ms);

                if state.baseline_iat_ms >= self.config.min_burst_baseline_ms
                    && iat_ms <= compressed_limit_ms
                {
                    state.compressed_streak += 1;
                    state.burst_min_iat_ms = state.burst_min_iat_ms.min(iat_ms);
                    let required_packets = if state.pending_gap_ms.is_some() {
                        self.config.post_stall_burst_packets
                    } else {
                        self.config.unprovoked_burst_packets
                    };
                    let burst_packets = state.compressed_streak + 1;
                    if burst_packets >= required_packets && !state.in_burst_episode {
                        state.in_burst_episode = true;
                        state.burst_events += 1;
                        self.heuristic_burst_events += 1;
                        events.push(UdpAnomalyEvent::HeuristicBurst {
                            remote_port,
                            local_port,
                            direction,
                            burst_packets,
                            preceding_gap_ms: state.pending_gap_ms.unwrap_or(0.0),
                            min_iat_ms: state.burst_min_iat_ms,
                        });
                        state.pending_gap_ms = None;
                    }
                } else {
                    state.compressed_streak = 0;
                    state.burst_min_iat_ms = f64::INFINITY;
                    state.in_burst_episode = false;
                    if iat_ms < gap_threshold_ms {
                        state.baseline_iat_ms = 0.85 * state.baseline_iat_ms + 0.15 * iat_ms;
                        state.pending_gap_ms = None;
                    }
                }
            }
        }

        events
    }

    /// Periodic tick checking active flows for mid-flow silence stalls or idle transitions.
    pub(crate) fn on_tick(&mut self, now: Duration) -> Vec<UdpAnomalyEvent> {
        let mut events = Vec::new();
        for (key, state) in &mut self.flows {
            let silence_ms = now.saturating_sub(state.last_seen).as_secs_f64() * 1000.0;
            if silence_ms >= self.config.idle_ms {
                state.phase = FlowPhase::Idle;
                state.pending_gap_ms = None;
                state.compressed_streak = 0;
                state.burst_min_iat_ms = f64::INFINITY;
                state.in_burst_episode = false;
                continue;
            }
            match state.phase {
                FlowPhase::Active
                    if state.baseline_iat_ms <= self.config.max_steady_baseline_ms =>
                {
                    let gap_threshold_ms = (self.config.gap_multiplier * state.baseline_iat_ms)
                        .max(self.config.min_gap_ms);
                    if silence_ms >= gap_threshold_ms {
                        state.phase = FlowPhase::Stalled { gap_reported: true };
                        state.gap_events += 1;
                        self.flow_gap_events += 1;
                        state.pending_gap_ms = Some(silence_ms);
                        events.push(UdpAnomalyEvent::FlowGap {
                            remote_port: key.remote_port,
                            local_port: key.local_port,
                            direction: key.direction,
                            gap_ms: silence_ms,
                            baseline_ms: state.baseline_iat_ms,
                        });
                    }
                }
                FlowPhase::Stalled { gap_reported: true } => {
                    state.pending_gap_ms = Some(silence_ms);
                }
                _ => {}
            }
        }
        events
    }

    /// Return aggregated session UDP summary counters.
    pub(crate) fn summary(&self) -> UdpSummary {
        UdpSummary {
            observed_ports: self.observed_ports.clone(),
            total_udp_packets: self.inbound_packets + self.outbound_packets,
            inbound_packets: self.inbound_packets,
            outbound_packets: self.outbound_packets,
            local_nic_drops: self.local_nic_drops,
            local_host_udp_drops: self.local_host_udp_drops,
            local_capture_drops: self.local_capture_drops,
            flow_gap_events: self.flow_gap_events,
            heuristic_burst_events: self.heuristic_burst_events,
        }
    }
}

/// Open an `AF_PACKET` raw socket with optional receive buffer sizing, timeout, and interface bind.
pub(crate) fn open_packet_socket(
    iface: Option<&str>,
    rcvbuf_bytes: Option<usize>,
    rcv_timeout_us: i64,
) -> io::Result<Fd> {
    let proto_be = (libc::ETH_P_ALL as u16).to_be();
    let fd = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_RAW, proto_be as libc::c_int) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let wrapped = Fd(fd);

    if let Some(bytes) = rcvbuf_bytes {
        let val = bytes as libc::c_int;
        unsafe {
            libc::setsockopt(
                wrapped.0,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }

    if let Some(name) = iface {
        let c_name = CString::new(name)
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "invalid interface name"))?;
        let idx = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
        if idx == 0 {
            let e = io::Error::last_os_error();
            return Err(if e.raw_os_error() == Some(0) {
                io::Error::new(ErrorKind::NotFound, format!("unknown interface: {name}"))
            } else {
                e
            });
        }
        let mut sll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        sll.sll_family = libc::AF_PACKET as u16;
        sll.sll_protocol = proto_be;
        sll.sll_ifindex = idx as libc::c_int;
        let rc = unsafe {
            libc::bind(
                wrapped.0,
                &sll as *const libc::sockaddr_ll as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
    }

    let rcv_to = libc::timeval {
        tv_sec: 0,
        tv_usec: rcv_timeout_us,
    };
    unsafe {
        libc::setsockopt(
            wrapped.0,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &rcv_to as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
    }

    Ok(wrapped)
}

/// Unwrap an Ethernet II (`0x0800`) or 802.1Q VLAN (`0x8100`) frame to its IPv4 payload slice.
pub(crate) fn unwrap_ip_payload(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() < 14 {
        return None;
    }
    match u16::from_be_bytes([frame[12], frame[13]]) {
        ETH_P_IP => Some(&frame[14..]),
        ETH_P_8021Q if frame.len() >= 18 && u16::from_be_bytes([frame[16], frame[17]]) == ETH_P_IP => {
            Some(&frame[18..])
        }
        _ => None,
    }
}

fn emit_anomaly_event(ev: &UdpAnomalyEvent) {
    match ev {
        UdpAnomalyEvent::PortDiscovered {
            remote_port,
            local_port,
            direction,
        } => {
            log_line(
                "UDP-PORT",
                &format!(
                    "port={} (local={}, {})",
                    remote_port,
                    local_port,
                    direction.label()
                ),
            );
        }
        UdpAnomalyEvent::FlowGap {
            remote_port,
            direction,
            gap_ms,
            baseline_ms,
            ..
        } => {
            log_line(
                "UDP-GAP",
                &format!(
                    "port={} dir={} gap={:.1}ms (baseline={:.1}ms)",
                    remote_port,
                    direction.label(),
                    gap_ms,
                    baseline_ms
                ),
            );
        }
        UdpAnomalyEvent::HeuristicBurst {
            remote_port,
            direction,
            burst_packets,
            preceding_gap_ms,
            min_iat_ms,
            ..
        } => {
            log_line(
                "UDP-BURST",
                &format!(
                    "port={} dir={} heuristic=order/burst pkts={} gap={:.1}ms min_iat={:.2}ms",
                    remote_port,
                    direction.label(),
                    burst_packets,
                    preceding_gap_ms,
                    min_iat_ms
                ),
            );
        }
    }
}

fn emit_drop_events(events: &[QueueDropEvent]) {
    for ev in events {
        log_line(
            "UDP-DROP",
            &format!("scope={} metric={} +{}", ev.scope, ev.metric, ev.delta),
        );
    }
}

/// Format the end-of-session `UDP <ip>:` summary line per `contracts/cli-output.md`.
pub(crate) fn format_udp_summary(target: Ipv4Addr, s: &UdpSummary) -> String {
    let ports = if s.observed_ports.is_empty() {
        "none".to_string()
    } else {
        s.observed_ports
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "UDP {}: ports=[{}] pkts={} (in={}, out={}) local_drops(nic={}, host={}, cap={}) flow_gaps={} heuristic_bursts={}",
        target,
        ports,
        s.total_udp_packets,
        s.inbound_packets,
        s.outbound_packets,
        s.local_nic_drops,
        s.local_host_udp_drops,
        s.local_capture_drops,
        s.flow_gap_events,
        s.heuristic_burst_events
    )
}

/// Capture and monitor live UDP packets for `target_ip` until `STOP` is signaled.
pub(crate) fn monitor_udp(
    target_ip: Ipv4Addr,
    initial_ports: BTreeSet<u16>,
    port_filter: Option<u16>,
    iface: Option<&str>,
) -> io::Result<UdpSummary> {
    let fd = open_packet_socket(iface, Some(CAPTURE_RCVBUF_BYTES), 50_000)?;
    let mut counters = LocalQueueCounters::baseline(iface, Some(fd.0));
    let mut tracker = UdpFlowTracker::new(target_ip, initial_ports, HeuristicConfig::default());

    let start = Instant::now();
    let mut next_tick = start + Duration::from_millis(POLL_TICK_MS);
    let mut frame_buf = vec![0u8; 65536];

    while !STOP.load(Ordering::SeqCst) {
        let mut sll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        let mut sll_len = std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
        let n = unsafe {
            libc::recvfrom(
                fd.0,
                frame_buf.as_mut_ptr() as *mut libc::c_void,
                frame_buf.len(),
                0,
                &mut sll as *mut _ as *mut libc::sockaddr,
                &mut sll_len,
            )
        };

        if n > 0 {
            if sll.sll_pkttype != libc::PACKET_OTHERHOST {
                if let Some(ip_slice) = unwrap_ip_payload(&frame_buf[..n as usize]) {
                    if let Ok(sliced) = SlicedPacket::from_ip(ip_slice) {
                        if let (
                            Some(NetSlice::Ipv4(v4)),
                            Some(TransportSlice::Udp(udp)),
                        ) = (&sliced.net, &sliced.transport)
                        {
                            let src_ip = v4.header().source_addr();
                            let dst_ip = v4.header().destination_addr();
                            let sp = udp.source_port();
                            let dp = udp.destination_port();
                            let is_excluded =
                                matches!(sp, 53 | 1900 | 5353) || matches!(dp, 53 | 1900 | 5353);
                            let explicitly_requested =
                                port_filter.is_some_and(|p| sp == p || dp == p);
                            if !is_excluded || explicitly_requested {
                                let events = tracker.on_packet(
                                    start.elapsed(),
                                    sll.sll_ifindex,
                                    src_ip,
                                    dst_ip,
                                    sp,
                                    dp,
                                );
                                for ev in &events {
                                    emit_anomaly_event(ev);
                                }
                            }
                        }
                    }
                }
            }
        } else {
            let e = io::Error::last_os_error();
            match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted => {}
                _ => return Err(e),
            }
        }

        let now_inst = Instant::now();
        if now_inst >= next_tick {
            let drop_events = counters.poll_deltas(iface, Some(fd.0));
            if !drop_events.is_empty() {
                tracker.record_queue_drops(&drop_events);
                emit_drop_events(&drop_events);
            }
            let tick_events = tracker.on_tick(start.elapsed());
            for ev in &tick_events {
                emit_anomaly_event(ev);
            }
            next_tick = now_inst + Duration::from_millis(POLL_TICK_MS);
        }
    }

    let final_drops = counters.poll_deltas(iface, Some(fd.0));
    if !final_drops.is_empty() {
        tracker.record_queue_drops(&final_drops);
        emit_drop_events(&final_drops);
    }

    Ok(tracker.summary())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv() -> Ipv4Addr {
        Ipv4Addr::new(92, 223, 1, 10)
    }

    fn cli() -> Ipv4Addr {
        Ipv4Addr::new(192, 168, 1, 50)
    }

    fn other() -> Ipv4Addr {
        Ipv4Addr::new(1, 2, 3, 4)
    }

    fn warm_up_flow(
        tracker: &mut UdpFlowTracker,
        ifindex: i32,
        dir: Direction,
        remote_port: u16,
        start_ms: u64,
        step_ms: u64,
        count: u64,
    ) -> u64 {
        let mut t = start_ms;
        for i in 0..count {
            t = start_ms + i * step_ms;
            let (src_ip, dst_ip, sp, dp) = match dir {
                Direction::Inbound => (srv(), cli(), remote_port, 50000),
                Direction::Outbound => (cli(), srv(), 50000, remote_port),
            };
            let evs = tracker.on_packet(Duration::from_millis(t), ifindex, src_ip, dst_ip, sp, dp);
            assert!(
                !evs.iter()
                    .any(|e| !matches!(e, UdpAnomalyEvent::PortDiscovered { .. })),
                "unexpected anomaly during warmup at step {i}: {evs:?}"
            );
        }
        t
    }

    #[test]
    fn filters_non_target_ips_and_detects_dynamic_new_ports() {
        let mut tracker = UdpFlowTracker::new(
            srv(),
            BTreeSet::from([32862]),
            HeuristicConfig::default(),
        );

        let ev_other = tracker.on_packet(
            Duration::from_millis(10),
            1,
            other(),
            cli(),
            32862,
            50000,
        );
        assert!(ev_other.is_empty());

        let ev_new_port = tracker.on_packet(
            Duration::from_millis(20),
            1,
            srv(),
            cli(),
            32863,
            50001,
        );
        assert_eq!(
            ev_new_port,
            vec![UdpAnomalyEvent::PortDiscovered {
                remote_port: 32863,
                local_port: 50001,
                direction: Direction::Inbound,
            }]
        );
        let summary = tracker.summary();
        assert_eq!(summary.observed_ports, BTreeSet::from([32862, 32863]));
        assert_eq!(summary.total_udp_packets, 1);
    }

    #[test]
    fn bidirectional_interleaving_and_bridge_ifindex_cause_zero_false_positives() {
        let mut tracker = UdpFlowTracker::new(
            srv(),
            BTreeSet::from([32862]),
            HeuristicConfig::default(),
        );

        for i in 0..30 {
            let t_in = i * 20;
            let t_out = i * 20 + 1;
            // Same packet seen on bridge (ifindex 2) and physical NIC (ifindex 3) 50us apart.
            let ev1 = tracker.on_packet(
                Duration::from_millis(t_in),
                2,
                srv(),
                cli(),
                32862,
                50000,
            );
            let ev2 = tracker.on_packet(
                Duration::from_micros(t_in * 1000 + 50),
                3,
                srv(),
                cli(),
                32862,
                50000,
            );
            let ev3 = tracker.on_packet(
                Duration::from_millis(t_out),
                2,
                cli(),
                srv(),
                50000,
                32862,
            );
            assert!(ev1.is_empty() && ev2.is_empty() && ev3.is_empty());
        }

        let summary = tracker.summary();
        assert_eq!(summary.flow_gap_events, 0);
        assert_eq!(summary.heuristic_burst_events, 0);
    }

    #[test]
    fn detects_mid_flow_gap_and_stall_then_burst_without_duplicate_alerts() {
        let mut tracker = UdpFlowTracker::new(
            srv(),
            BTreeSet::from([32862]),
            HeuristicConfig::default(),
        );
        let last = warm_up_flow(&mut tracker, 1, Direction::Inbound, 32862, 0, 20, 12);

        // Tick at +200ms detects mid-flow silence (> 150ms).
        let tick_evs = tracker.on_tick(Duration::from_millis(last + 200));
        assert_eq!(tick_evs.len(), 1);
        assert!(matches!(tick_evs[0], UdpAnomalyEvent::FlowGap { .. }));

        // At +300ms, 5 packets arrive 0.5ms apart: must emit exactly ONE HeuristicBurst.
        let mut burst_alerts = 0;
        for j in 0..5 {
            let ts = Duration::from_micros((last + 300) * 1000 + j * 500);
            let evs = tracker.on_packet(ts, 1, srv(), cli(), 32862, 50000);
            burst_alerts += evs
                .iter()
                .filter(|e| matches!(e, UdpAnomalyEvent::HeuristicBurst { .. }))
                .count();
        }
        assert_eq!(burst_alerts, 1);

        let summary = tracker.summary();
        assert_eq!(summary.flow_gap_events, 1);
        assert_eq!(summary.heuristic_burst_events, 1);
    }

    #[test]
    fn suppresses_alerts_when_flow_becomes_idle() {
        let mut tracker = UdpFlowTracker::new(
            srv(),
            BTreeSet::from([32862]),
            HeuristicConfig::default(),
        );
        let last = warm_up_flow(&mut tracker, 1, Direction::Inbound, 32862, 0, 20, 12);

        // Extended pause > 2000ms moves flow directly to Idle without gap alert.
        let evs_idle = tracker.on_tick(Duration::from_millis(last + 2500));
        assert!(evs_idle.is_empty());

        // Subsequent tick while still idle stays quiet.
        let evs_still_idle = tracker.on_tick(Duration::from_millis(last + 4000));
        assert!(evs_still_idle.is_empty());

        // Resuming after idle re-warms without triggering gap or burst.
        let _ = warm_up_flow(&mut tracker, 1, Direction::Inbound, 32862, last + 5000, 20, 6);
        let summary = tracker.summary();
        assert_eq!(summary.flow_gap_events, 0);
        assert_eq!(summary.heuristic_burst_events, 0);
    }

    #[test]
    fn formats_udp_summary_for_populated_and_empty_ports() {
        let empty = UdpSummary::default();
        assert_eq!(
            format_udp_summary(Ipv4Addr::new(1, 1, 1, 1), &empty),
            "UDP 1.1.1.1: ports=[none] pkts=0 (in=0, out=0) local_drops(nic=0, host=0, cap=0) flow_gaps=0 heuristic_bursts=0"
        );

        let populated = UdpSummary {
            observed_ports: BTreeSet::from([32862, 32863]),
            total_udp_packets: 120,
            inbound_packets: 70,
            outbound_packets: 50,
            local_nic_drops: 2,
            local_host_udp_drops: 1,
            local_capture_drops: 0,
            flow_gap_events: 3,
            heuristic_burst_events: 1,
        };
        assert_eq!(
            format_udp_summary(srv(), &populated),
            "UDP 92.223.1.10: ports=[32862,32863] pkts=120 (in=70, out=50) local_drops(nic=2, host=1, cap=0) flow_gaps=3 heuristic_bursts=1"
        );
    }

    #[test]
    fn unwraps_ethernet_and_vlan_frames() {
        let mut eth = vec![0u8; 20];
        eth[12..14].copy_from_slice(&ETH_P_IP.to_be_bytes());
        eth[14] = 0x45;
        assert_eq!(unwrap_ip_payload(&eth).map(|s| s[0]), Some(0x45));

        let mut vlan = vec![0u8; 24];
        vlan[12..14].copy_from_slice(&ETH_P_8021Q.to_be_bytes());
        vlan[16..18].copy_from_slice(&ETH_P_IP.to_be_bytes());
        vlan[18] = 0x45;
        assert_eq!(unwrap_ip_payload(&vlan).map(|s| s[0]), Some(0x45));

        assert!(unwrap_ip_payload(&[0u8; 10]).is_none());
    }

    #[test]
    fn detects_unprovoked_compressed_burst_cluster() {
        let mut tracker = UdpFlowTracker::new(
            srv(),
            BTreeSet::from([32862]),
            HeuristicConfig::default(),
        );
        let last = warm_up_flow(&mut tracker, 1, Direction::Outbound, 32862, 0, 20, 12);

        let mut burst_alerts = 0;
        for j in 1..=4 {
            let ts = Duration::from_micros(last * 1000 + j * 400);
            let evs = tracker.on_packet(ts, 1, cli(), srv(), 50000, 32862);
            burst_alerts += evs
                .iter()
                .filter(|e| matches!(e, UdpAnomalyEvent::HeuristicBurst { .. }))
                .count();
        }
        assert_eq!(burst_alerts, 1);
    }
}
