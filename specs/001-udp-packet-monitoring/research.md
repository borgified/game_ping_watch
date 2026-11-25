# Research: UDP Packet Drop and Order Monitoring

**Feature**: `001-udp-packet-monitoring` | **Date**: 2026-11-25

## 1. Concurrent ICMP Ping and UDP Packet Capture Architecture

- **Decision**: Run the UDP monitor (`AF_PACKET` capture loop plus periodic local queue counter and flow-stall ticks) on a dedicated worker thread (`std::thread::scope` or `std::thread::spawn`) concurrently with the existing ICMP `ping_loop`, sharing the existing `STOP: AtomicBool` shutdown flag and returning a structured `UdpSummary` upon thread join.
- **Rationale**:
  - `ping_loop` relies on a tight `20ms` `SO_RCVTIMEO` loop to measure ICMP RTT accurately and emit `[TIMEOUT]` notifications promptly. Running heavy UDP packet bursts in the same single-threaded `poll()` loop risks delaying ICMP `recvfrom` timestamps or `sendto` cadence.
  - Using a dedicated thread with a `50ms` `SO_RCVTIMEO` on `AF_PACKET` keeps ICMP RTT precision intact, handles `Ctrl+C` cleanly within `<= 50ms`, and needs zero external async runtimes (keeping `Cargo.toml` unchanged).
  - Rust's `println!` locks `std::io::stdout()` per call, ensuring individual timestamped alert lines never interleave mid-line.
- **Alternatives Considered**:
  - *Single-threaded `poll(2)` multiplexing ICMP and `AF_PACKET` sockets*: Rejected because high-rate UDP bursts could starve or skew ICMP RTT timestamping unless complex fair-draining budgets are introduced.
  - *Spawning an external `tcpdump`/`tshark` subprocess*: Rejected due to external binary dependency, text-parsing fragility, and loss of unified in-process summary metrics.

## 2. Local Queue Drop Telemetry Sources on Linux

- **Decision**: Poll three complementary Linux drop counters at a fixed interval (e.g., every `500ms`) and compute deltas from session start baselines:
  1. **Capture socket ring drops**: `getsockopt(fd, SOL_PACKET, PACKET_STATISTICS, ...)` (`libc::tpacket_stats`), which reports `tp_drops` (packets dropped by the kernel packet socket buffer since the previous call, resetting atomically on read). Also configure `SO_RCVBUF` (e.g., `4 * 1024 * 1024` bytes) to minimize observer-induced drops.
  2. **Host UDP socket buffer drops**: Parse `/proc/net/snmp` by matching header column names on the `Udp:` header/value lines for `RcvbufErrors`, `SndbufErrors`, `InErrors`, and `InCsumErrors`.
  3. **Network interface queue drops**: Read `/sys/class/net/<iface>/statistics/{rx_dropped,tx_dropped,rx_missed_errors,rx_fifo_errors,tx_fifo_errors}` for the selected interface (or all non-loopback active interfaces when `iface` is `None`).
- **Rationale**:
  - Each counter observes a different queue layer: NIC hardware/driver RX/TX ring (`sysfs`), kernel UDP socket receive buffer (`/proc/net/snmp`), and `AF_PACKET` monitor ring (`PACKET_STATISTICS`).
  - Parsing `/proc/net/snmp` by column name (verified on Ubuntu 24.04 output) protects against kernel version differences in column positions.
  - Labeling alerts by exact counter scope (`nic:<iface>`, `host:udp`, `capture:af_packet`) keeps system-wide counters distinct from target-IP-specific flow gaps.
- **Alternatives Considered**:
  - *Netlink `rtnetlink` / `sock_diag` polling*: Rejected because `sysfs` and `/proc/net/snmp` require no extra Netlink framing/parsing code and are trivially unit-testable from mock text strings.
  - *Relying only on `PACKET_STATISTICS`*: Rejected because `PACKET_STATISTICS` only sees capture-socket overflows, missing actual game socket `RcvbufErrors` or NIC `rx_dropped`.

## 3. Port Association Scope and Discovery Handoff

- **Decision**: Extend `discover_ip()` to return a `DiscoveryResult` containing both the top-ranked `Ipv4Addr` and the set of all UDP remote ports (and observed local ports) exchanged with that IP during discovery, while filtering out TCP-only ports for UDP tracking. During the monitoring phase, track all UDP packets matching `target_ip` across any port (automatically adding newly observed UDP ports for `target_ip` and emitting a `[UDP-PORT]` info line when a new port appears after discovery), while respecting discovery port filtering semantics cleanly.
- **Rationale**:
  - Satisfies `FR-001` and `FR-003`: games often use one primary port for match state and secondary ephemeral/control UDP ports with the same server IP; tracking all UDP ports communicating with `target_ip` ensures mid-match port switches are never missed.
  - Wait: what if the user passed a specific discovery port (e.g., `32862`)? During discovery, that filter narrows which server IP wins discovery; once the server IP is chosen, monitoring all UDP ports communicating with that `target_ip` fulfills "monitor UDP packets ... on those ports associated by the detected ip".
- **Alternatives Considered**:
  - *Freezing the UDP port list strictly at discovery end*: Rejected because games frequently allocate match or voice/telemetry UDP ports right after lobby/server handshake.

## 4. Bridge Duplicate Frame Handling and Direction Separation

- **Decision**:
  - Determine packet direction from the IPv4 header relative to `target_ip`: `src == target_ip` is `Inbound`, `dst == target_ip` is `Outbound`.
  - Key each monitored UDP flow by `(ifindex, local_port, remote_port, direction)`, reading `sll_ifindex` via `recvfrom(..., &mut sll, &mut len)` on `AF_PACKET` (and skipping `sll_pkttype == PACKET_OTHERHOST`).
- **Rationale**:
  - On hosts using a Linux bridge (such as `br0` + `enp37s0`, documented in `README.md`), an unbound `AF_PACKET` socket (`iface = None`) receives the same packet twice — once on the physical NIC and once on the bridge — ~microseconds apart. Without `sll_ifindex` in the flow key, a single packet traversing the bridge would look like a zero-microsecond burst!
  - Including `sll_ifindex` in the flow key (or binding to a single `iface` when specified) prevents cross-interface duplicates from corrupting inter-arrival statistics while keeping inbound and outbound cadence completely independent (`FR-007`).
- **Alternatives Considered**:
  - *Packet payload hashing for deduplication across interfaces*: Rejected as unnecessary CPU overhead per packet when keying by `sll_ifindex` cleanly separates per-interface observations.

## 5. Arrival-Timing and Burst Heuristic Design

- **Decision**: Implement a deterministic per-flow state machine (`UdpFlowState`) using monotonic timestamps (`Duration` since session start for pure testability):
  - **Baseline Cadence (Warm-up)**: Collect the first `10` inter-arrival intervals (`iat`) on a directional flow before enabling alerts. Maintain an Exponentially Weighted Moving Average (`baseline_iat`) updated during steady packets (`iat < idle_threshold`) so gradual tick-rate changes adapt smoothly.
  - **Idle Boundary**: If `iat >= 2000 ms` (idle threshold), transition the flow to `Idle` and re-warm (`5` packets) on resumption instead of emitting repeated stall/gap alerts during loading screens or menu pauses.
  - **Flow Delivery Gap (`[UDP-GAP]`)**: When an active steady flow (`baseline_iat <= 250 ms` and warmed up) experiences silence `>= max(150 ms, 4.0 * baseline_iat)` and `< 2000 ms` (checked both on packet arrival and on the `500ms` periodic timer tick if a flow goes silent mid-stream), record a flow-gap event (at most one gap alert per stall episode until packets resume).
  - **Suspected Reordering / Queued Burst (`[UDP-BURST]`)**: Flag a heuristic burst anomaly when either:
    1. A flow recovers from a stall (`gap >= max(100 ms, 3.0 * baseline_iat)`) and receives `>= 3` consecutive packets with compressed spacing (`iat <= max(2.0 ms, 0.25 * baseline_iat)`), OR
    2. Without a prior large gap, `>= 4` consecutive packets arrive with compressed spacing (`iat <= max(1.5 ms, 0.20 * baseline_iat)`) while `baseline_iat >= 10 ms` (indicative of upstream queueing/reordering release).
    Emit one `[UDP-BURST]` alert per burst episode (with burst packet count and stall duration) and clearly label all heuristic outputs as inferred timing/burst anomalies rather than proven sequence swaps (`FR-006`).
- **Rationale**:
  - Encapsulates all thresholds in a pure struct (`HeuristicConfig` + `UdpFlowTracker`) with zero OS dependencies, enabling deterministic unit tests for steady streams, bidirectional interleaving, stalls, bursts, and idle pauses.
  - Guards against low-rate keepalive flows (`baseline_iat > 250 ms`) and natural sub-millisecond game state multi-packet ticks (`baseline_iat < 10 ms`) so steady gameplay does not false-trigger.
- **Alternatives Considered**:
  - *Fixed global millisecond thresholds without per-flow baseline*: Rejected because different games run at different tick rates (e.g., 20 Hz = 50ms vs. 60 Hz = 16.6ms vs. 128 Hz = 7.8ms); relative multiples of per-flow `baseline_iat` adapt automatically.
