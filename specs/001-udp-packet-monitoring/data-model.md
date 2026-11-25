# Data Model: UDP Packet Drop and Order Monitoring

**Feature**: `001-udp-packet-monitoring` | **Date**: 2026-11-25

## Entities

### 1. `DiscoveryResult`
Output of the initial discovery capture phase handed off to the concurrent monitoring phase.

- `target_ip: Ipv4Addr` — Top-ranked public IPv4 server address selected by total byte volume.
- `udp_ports: BTreeSet<u16>` — Sorted set of remote UDP ports observed communicating with `target_ip` during discovery (may be empty if discovery saw only TCP traffic for `target_ip`).
- `total_packets: u64` — Total packets observed for `target_ip` during discovery.
- `total_bytes: u64` — Total bytes observed for `target_ip` during discovery.

**Validation Rules**:
- `target_ip` MUST be a public IPv4 address (not loopback, private, multicast, broadcast, link-local, or unspecified).
- `udp_ports` excludes discovery-ignored ports (`53`, `1900`, `5353`) unless explicitly requested via the CLI port filter.

---

### 2. `Direction`
Traffic direction relative to the local host and `target_ip`.

- `Inbound` — `src_ip == target_ip` (server to local host).
- `Outbound` — `dst_ip == target_ip` (local host to server).

---

### 3. `UdpFlowKey`
Unique directional identifier for a monitored UDP packet stream.

- `ifindex: i32` — Linux interface index (`sll_ifindex`, or `0` in pure unit tests) preventing bridge duplicate cross-talk.
- `local_port: u16` — Local host UDP port.
- `remote_port: u16` — Remote `target_ip` UDP port.
- `direction: Direction` — `Inbound` or `Outbound`.

**Relationships**:
- Each `UdpFlowKey` maps 1:1 to a `UdpFlowState` inside `UdpFlowTracker`.
- Multiple `UdpFlowKey` entries can share a single `remote_port` (e.g., `Inbound` vs `Outbound`, or multiple local ephemeral sockets).

---

### 4. `FlowPhase` & `UdpFlowState`
Per-flow cadence baseline and anomaly detection state machine.

- `phase: FlowPhase`:
  - `WarmingUp { samples: u8 }` — Initial window (`10` intervals on first appearance, `5` intervals after idle recovery) before emitting gap/burst alerts.
  - `Active` — Steady cadence established; gap and burst heuristics enabled.
  - `Stalled { gap_reported: bool }` — Active flow currently exceeding gap threshold without hitting idle timeout.
  - `Idle` — Flow silent for `>= idle_threshold` (`2000ms`); suppresses stall alerts until traffic resumes.
- `packet_count: u64` — Total UDP packets observed on this directional flow.
- `last_seen: Duration` — Monotonic session timestamp of most recent packet.
- `baseline_iat_ms: f64` — Smoothed inter-arrival time (EWMA) in milliseconds.
- `pending_gap_ms: Option<f64>` — Most recent stall duration preceding recovery, used to classify post-stall bursts.
- `compressed_streak: u32` — Consecutive compressed-spacing packets (`iat <= max(2.0ms, 0.25 * baseline_iat_ms)`).
- `in_burst_episode: bool` — Latch preventing duplicate alerts during a single burst cluster.
- `gap_events: u64` — Count of `[UDP-GAP]` events on this flow.
- `burst_events: u64` — Count of `[UDP-BURST]` heuristic events on this flow.

**State Transitions**:
```text
  [New Flow] ──(1st packet)──> WarmingUp(samples=0)
  WarmingUp  ──(samples >= warmup_target)──> Active
  WarmingUp  ──(gap >= idle_ms)──> Idle
  Active     ──(silence >= gap_threshold && < idle_ms)──> Stalled(gap_reported=true) [emits UDP-GAP]
  Active     ──(gap >= idle_ms)──> Idle
  Stalled    ──(packet arrives < idle_ms)──> Active [records pending_gap_ms; evaluates UDP-BURST]
  Stalled    ──(silence >= idle_ms)──> Idle
  Idle       ──(packet arrives)──> WarmingUp(samples=0, target=re_warmup_samples)
```

---

### 5. `LocalQueueCounters` & `QueueDropDelta`
Periodic snapshot of host, NIC, and capture-socket drop counters.

- `nic_drops: BTreeMap<String, NicDropSnapshot>` — Per-interface cumulative counters (`rx_dropped`, `tx_dropped`, `rx_missed_errors`, `rx_fifo_errors`, `tx_fifo_errors`) read from `/sys/class/net/<iface>/statistics/`.
- `host_udp: HostUdpSnapshot` — System-wide UDP counters (`rcvbuf_errors`, `sndbuf_errors`, `in_errors`, `in_csum_errors`) parsed from `/proc/net/snmp`.
- `capture_drops: u64` — Cumulative `tp_drops` accumulated from `PACKET_STATISTICS` `getsockopt` calls on the `AF_PACKET` socket.

**Validation Rules**:
- Deltas use saturating subtraction (`new.saturating_sub(prev)`) to remain safe against counter resets.
- Each non-zero delta generates a `UdpAnomalyEvent::LocalQueueDrop` identifying exact scope (`nic:<iface>`, `host:udp`, or `capture:af_packet`).

---

### 6. `UdpAnomalyEvent`
Timestamped diagnostic event emitted in real time and counted in the session summary.

- `LocalQueueDrop { timestamp, scope, metric, delta }` — Confirmed queue overflow on local NIC, host UDP buffer, or capture ring.
- `FlowGap { timestamp, remote_port, local_port, direction, gap_ms, baseline_ms }` — Unexpected mid-flow silence relative to active flow cadence.
- `HeuristicBurst { timestamp, remote_port, local_port, direction, burst_packets, preceding_gap_ms, min_iat_ms }` — Suspected out-of-order/queued-burst delivery inferred from arrival-timing heuristics.
- `PortDiscovered { timestamp, remote_port }` — Informational notice when a new UDP port for `target_ip` appears after discovery ends.

---

### 7. `UdpSummary` & `SessionDiagnosticSummary`
End-of-session aggregated metrics printed on `Ctrl+C` alongside ICMP ping totals.

- `observed_ports: BTreeSet<u16>` — All remote UDP ports observed for `target_ip` across discovery and monitoring.
- `total_udp_packets: u64` — Total UDP packets monitored for `target_ip` (`inbound_packets + outbound_packets`).
- `inbound_packets: u64` / `outbound_packets: u64` — Per-direction UDP packet totals.
- `local_nic_drops: u64` — Sum of NIC drop deltas during monitoring.
- `local_host_udp_drops: u64` — Sum of host UDP buffer/error deltas during monitoring.
- `local_capture_drops: u64` — Sum of `AF_PACKET` ring drops during monitoring.
- `flow_gap_events: u64` — Total `[UDP-GAP]` events across monitored flows.
- `heuristic_burst_events: u64` — Total `[UDP-BURST]` heuristic events across monitored flows.
