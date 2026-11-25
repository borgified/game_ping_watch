---

description: "Task list for UDP Packet Drop and Order Monitoring"
---

# Tasks: UDP Packet Drop and Order Monitoring

**Input**: Design documents from `/specs/001-udp-packet-monitoring/`
**Prerequisites**: [plan.md](file:///home/fwiffo/projects/game_ping_watch/specs/001-udp-packet-monitoring/plan.md), [spec.md](file:///home/fwiffo/projects/game_ping_watch/specs/001-udp-packet-monitoring/spec.md), [research.md](file:///home/fwiffo/projects/game_ping_watch/specs/001-udp-packet-monitoring/research.md), [data-model.md](file:///home/fwiffo/projects/game_ping_watch/specs/001-udp-packet-monitoring/data-model.md), [cli-output.md](file:///home/fwiffo/projects/game_ping_watch/specs/001-udp-packet-monitoring/contracts/cli-output.md), [quickstart.md](file:///home/fwiffo/projects/game_ping_watch/specs/001-udp-packet-monitoring/quickstart.md)

**Tests**: Inline Rust unit tests (`cargo test`) are included per user story so pure discovery, flow-tracking, heuristic, and counter logic can be verified without root privileges.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., `US1`, `US2`, `US3`, `US4`)
- Exact file paths are included in every task description.

## Path Conventions

- Single-project Rust binary layout at repository root (`Cargo.toml`, `src/*.rs`).

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Modularize the single-file crate structure per `plan.md` without changing existing CLI behavior.

- [X] T001 Create module files `src/discovery.rs`, `src/ping.rs`, `src/udp_monitor.rs`, and `src/queue_counters.rs` and declare them in `src/main.rs`
- [X] T002 [P] Expose shared `Fd`, `ts_now()`, `is_pub_v4()`, and `STOP` utilities for reuse across modules in `src/main.rs`
- [X] T003 [P] Move existing ICMP `ping_loop()` and `checksum()` from `src/main.rs` into `src/ping.rs` with identical behavior and output

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core models, `AF_PACKET` socket capture helpers, and concurrent execution wiring required by all user stories.

**⚠️ CRITICAL**: Complete this phase before beginning user story implementation.

- [X] T004 Define core UDP monitoring types (`Direction`, `UdpFlowKey`, `FlowPhase`, `UdpFlowState`, `UdpAnomalyEvent`, `UdpSummary`) in `src/udp_monitor.rs`, referencing `data-model.md` (`UdpFlowKey.ifindex`, `UdpFlowKey.local_port`, `UdpFlowKey.remote_port`, `UdpFlowKey.direction`, `UdpFlowState.phase`, `UdpFlowState.packet_count`, `UdpFlowState.last_seen`, `UdpFlowState.baseline_iat_ms`, `UdpFlowState.pending_gap_ms`, `UdpFlowState.compressed_streak`, `UdpFlowState.in_burst_episode`, `UdpFlowState.gap_events`, `UdpFlowState.burst_events`, `UdpSummary.observed_ports`, `UdpSummary.total_udp_packets`, `UdpSummary.inbound_packets`, `UdpSummary.outbound_packets`, `UdpSummary.local_nic_drops`, `UdpSummary.local_host_udp_drops`, `UdpSummary.local_capture_drops`, `UdpSummary.flow_gap_events`, `UdpSummary.heuristic_burst_events`)
- [X] T005 [P] Define `DiscoveryResult` in `src/discovery.rs` citing `data-model.md` (`DiscoveryResult.target_ip`, `DiscoveryResult.udp_ports`, `DiscoveryResult.total_packets`, `DiscoveryResult.total_bytes`)
- [X] T006 [P] Implement reusable `AF_PACKET` socket setup (`SO_RCVBUF` sizing, `SO_RCVTIMEO`, optional `sockaddr_ll` interface binding) and `recvfrom` with `sockaddr_ll` metadata (`sll_ifindex`, `sll_pkttype`) in `src/udp_monitor.rs`
- [X] T007 Wire concurrent `std::thread` execution of the UDP monitor alongside `ping::ping_loop()` in `src/main.rs`, coordinating shutdown via `STOP` and joining the monitor thread before exit

**Checkpoint**: Foundation ready — user story phases can now execute cleanly.

---

## Phase 3: User Story 1 - Track All UDP Ports Associated With the Detected Server IP (Priority: P1) 🎯 MVP

**Goal**: Automatically collect all UDP ports communicating with the selected server IP during discovery, monitor only that IP across all associated ports, and dynamically register new UDP ports opened with that IP during monitoring.

**Independent Test**: Run `cargo test` in `src/discovery.rs` and `src/udp_monitor.rs` verifying multi-port collection, non-target IP filtering, excluded discovery ports, dynamic port addition, and TCP-only fallback.

### Implementation for User Story 1

- [X] T008 [US1] Refactor `discover_ip()` into `src/discovery.rs` to aggregate all remote UDP ports per public IPv4 candidate (citing `data-model.md` `DiscoveryResult.target_ip` and `DiscoveryResult.udp_ports`) and return `io::Result<Option<DiscoveryResult>>`
- [X] T009 [US1] Update discovery `stderr` reporting in `src/discovery.rs` to print `Selected target: <ip> (associated UDP ports: ...)` matching `contracts/cli-output.md`
- [X] T010 [US1] Implement `target_ip` packet filtering (skipping `PACKET_OTHERHOST` and non-target IPs), 802.1Q VLAN unwrapping, and dynamic post-discovery port registration emitting `[UDP-PORT]` (`UdpAnomalyEvent::PortDiscovered` in `data-model.md`) in `src/udp_monitor.rs`
- [X] T011 [US1] Handle empty `DiscoveryResult.udp_ports` (TCP-only discovery) gracefully in `src/main.rs` and `src/udp_monitor.rs` by running `ping::ping_loop()` normally and staying ready for later UDP traffic per `contracts/cli-output.md`
- [X] T012 [P] [US1] Add unit tests for multi-port discovery, discovery port exclusions (`53`, `1900`, `5353`), non-target IP filtering, dynamic port discovery, and zero-UDP fallback in `src/discovery.rs` and `src/udp_monitor.rs`

**Checkpoint**: User Story 1 is independently functional and unit-testable (MVP port tracking ready).

---

## Phase 4: User Story 2 - Detect UDP Queue Drops During Gameplay (Priority: P1)

**Goal**: Detect and alert in real time on both confirmed local queue overflows (NIC sysfs, `/proc/net/snmp`, `PACKET_STATISTICS`) and mid-flow UDP delivery gaps (`[UDP-GAP]`) concurrently with the ICMP ping loop.

**Independent Test**: Run `cargo test` in `src/queue_counters.rs` and `src/udp_monitor.rs` verifying `/proc/net/snmp` column parsing, saturating drop deltas, warm-up suppression, mid-flow gap triggers, and idle suppression.

### Implementation for User Story 2

- [X] T013 [P] [US2] Implement `LocalQueueCounters`, `NicDropSnapshot`, and `HostUdpSnapshot` readers (column-matched `/proc/net/snmp` UDP errors, `/sys/class/net/<iface>/statistics/` drops, and `PACKET_STATISTICS` `tp_drops`) with saturating delta calculation in `src/queue_counters.rs`, citing `data-model.md` (`LocalQueueCounters.nic_drops`, `LocalQueueCounters.host_udp`, `LocalQueueCounters.capture_drops`, `UdpAnomalyEvent::LocalQueueDrop`)
- [X] T014 [US2] Implement per-flow warm-up baseline (`baseline_iat_ms`), idle threshold transition (`FlowPhase::Idle`), and mid-flow silence gap detection (`FlowPhase::Stalled`, `UdpAnomalyEvent::FlowGap` in `data-model.md`) checked on both packet arrival and periodic timer ticks in `src/udp_monitor.rs`
- [X] T015 [US2] Integrate `500ms` periodic queue counter polling and flow-stall checks into the `AF_PACKET` loop in `src/udp_monitor.rs`, emitting timestamped `[UDP-DROP]` and `[UDP-GAP]` alerts per `contracts/cli-output.md`
- [X] T016 [P] [US2] Add unit tests for `/proc/net/snmp` parsing, sysfs/capture saturating drop deltas, warm-up suppression, steady-cadence gap detection, and idle-pause suppression in `src/queue_counters.rs` and `src/udp_monitor.rs`

**Checkpoint**: User Stories 1 and 2 work together cleanly — port tracking, local drop alerts, and flow-gap detection are verified.

---

## Phase 5: User Story 3 - Detect Out-of-Order / Bursty UDP Delivery via Timing Heuristics (Priority: P2)

**Goal**: Analyze directional UDP packet arrival timing and burst clusters per `(ifindex, local_port, remote_port, direction)` flow to emit labeled heuristic alerts without false positives on bidirectional or bridged traffic.

**Independent Test**: Run `cargo test` in `src/udp_monitor.rs` feeding simulated directional packet timelines (bidirectional interleaving, multi-interface bridge duplicates, stall-then-burst, compressed packet cluster) into `UdpFlowTracker`.

### Implementation for User Story 3

- [X] T017 [US3] Enforce directional flow isolation using `UdpFlowKey` (`ifindex`, `local_port`, `remote_port`, `direction`) derived from IPv4 `src`/`dst` vs `target_ip` and `sll_ifindex` in `src/udp_monitor.rs`, citing `data-model.md` (`Direction`, `UdpFlowKey.ifindex`, `UdpFlowKey.direction`)
- [X] T018 [US3] Implement post-stall burst and compressed-cluster heuristic detection (`UdpFlowState.pending_gap_ms`, `UdpFlowState.compressed_streak`, `UdpFlowState.in_burst_episode`, `UdpFlowState.burst_events`, `UdpAnomalyEvent::HeuristicBurst` in `data-model.md`) emitting `[UDP-BURST] ... heuristic=order/burst` alerts per `contracts/cli-output.md` in `src/udp_monitor.rs`
- [X] T019 [P] [US3] Add unit tests confirming zero false positives for interleaved bidirectional traffic and separate `ifindex` bridge frames, plus accurate single-alert-per-episode behavior on stall-then-burst and compressed clusters in `src/udp_monitor.rs`

**Checkpoint**: User Stories 1, 2, and 3 are independently testable via `cargo test`.

---

## Phase 6: User Story 4 - Correlated Real-Time Output and Session Summary (Priority: P2)

**Goal**: Present unified chronological startup banners, real-time ICMP + UDP alert formatting, and the combined `Ctrl+C` end-of-session summary.

**Independent Test**: Run unit tests for `UdpSummary` formatting and run a live `sudo` session verifying chronological alert output and combined `Summary` + `RTT ms` + `UDP <ip>:` output on `Ctrl+C`.

### Implementation for User Story 4

- [X] T020 [US4] Update startup banner output across `src/main.rs` and `src/ping.rs` to print `Pinging <ip>...` and `Monitoring UDP for <ip> on ports [...]` per `contracts/cli-output.md`
- [X] T021 [US4] Implement `UdpSummary` formatting and print the `UDP <ip>:` summary line immediately after the ICMP `Summary` and `RTT ms` lines upon `SIGINT` completion in `src/main.rs` and `src/udp_monitor.rs`, citing `data-model.md` (`UdpSummary.observed_ports`, `UdpSummary.total_udp_packets`, `UdpSummary.inbound_packets`, `UdpSummary.outbound_packets`, `UdpSummary.local_nic_drops`, `UdpSummary.local_host_udp_drops`, `UdpSummary.local_capture_drops`, `UdpSummary.flow_gap_events`, `UdpSummary.heuristic_burst_events`)
- [X] T022 [P] [US4] Add unit tests verifying `UdpSummary` aggregation and exact `contracts/cli-output.md` formatting (including empty port list `ports=[none]`) in `src/udp_monitor.rs`

**Checkpoint**: All four user stories are implemented, integrated, and covered by unit tests.

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, compiler cleanliness, and end-to-end validation across all scenarios.

- [X] T023 [P] Update [README.md](file:///home/fwiffo/projects/game_ping_watch/README.md) and [CHANGELOG.md](file:///home/fwiffo/projects/game_ping_watch/CHANGELOG.md) to document UDP port tracking, `[UDP-DROP]`, `[UDP-GAP]`, `[UDP-BURST]`, `[UDP-PORT]`, and the combined summary output
- [X] T024 Run `cargo test`, `cargo build --release`, and validate live scenarios per [quickstart.md](file:///home/fwiffo/projects/game_ping_watch/specs/001-udp-packet-monitoring/quickstart.md)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately.
- **Foundational (Phase 2)**: Depends on Phase 1 (`T001`–`T003`) — blocks all user story phases.
- **User Story 1 (Phase 3 - P1)**: Depends on Phase 2 (`T004`–`T007`). Provides discovery-to-monitor port handoff.
- **User Story 2 (Phase 4 - P1)**: Depends on Phase 2 (`T004`–`T007`). `T013` can run in parallel with User Story 1; `T015` integrates into `src/udp_monitor.rs` alongside `T010`.
- **User Story 3 (Phase 5 - P2)**: Depends on Phase 2 (`T004`) and builds on the flow state tracking in `src/udp_monitor.rs` (`T014`).
- **User Story 4 (Phase 6 - P2)**: Depends on Phases 3–5 (`T008`–`T019`) to aggregate all summary counters and banners.
- **Polish (Phase 7)**: Depends on all desired user stories being complete.

### Parallel Opportunities

- **Phase 1**: `T002` and `T003` can run in parallel after `T001`.
- **Phase 2**: `T005` (`src/discovery.rs`) and `T006` (`src/udp_monitor.rs`) can run in parallel with/after `T004`.
- **Phase 3 & Phase 4**: `T013` (`src/queue_counters.rs`) has no file overlap with `T008`–`T010` and can run in parallel with User Story 1.
- **Per-Story Tests**: `T012`, `T016`, `T019`, `T022` can be authored in parallel with or right before implementation in their respective modules.

---

## Parallel Example: Foundational + Early US2 Counter Work

```bash
# Run independent module tasks in parallel after Phase 1:
Task: "T005 [P] Define DiscoveryResult in src/discovery.rs"
Task: "T006 [P] Implement AF_PACKET socket setup helper in src/udp_monitor.rs"
Task: "T013 [P] [US2] Implement LocalQueueCounters parsing in src/queue_counters.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (`T001`–`T003`).
2. Complete Phase 2: Foundational (`T004`–`T007`).
3. Complete Phase 3: User Story 1 (`T008`–`T012`).
4. **STOP and VALIDATE**: Run `cargo test` and verify multi-port discovery and target-IP UDP filtering.

### Incremental Delivery

1. Setup + Foundational → modular crate structure ready.
2. Add User Story 1 → target IP + associated UDP ports tracked dynamically (MVP).
3. Add User Story 2 → confirmed local queue drops + mid-flow delivery gaps alerted live.
4. Add User Story 3 → directional timing/burst heuristics detect suspected reordering without false positives.
5. Add User Story 4 → unified startup banner and combined `Ctrl+C` summary output.
6. Polish (`T023`–`T024`) → documentation and full `quickstart.md` validation.
