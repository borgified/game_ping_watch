# Implementation Plan: UDP Packet Drop and Order Monitoring

**Branch**: `001-udp-packet-monitoring` | **Date**: 2026-11-25 | **Spec**: [spec.md](file:///home/fwiffo/projects/game_ping_watch/specs/001-udp-packet-monitoring/spec.md)

**Input**: Feature specification from `/specs/001-udp-packet-monitoring/spec.md`

## Summary

Extend `game_ping_watch` so the discovery phase captures all UDP ports associated with the top-ranked public IPv4 server address and hands them to a concurrent UDP monitor that runs alongside the existing ICMP ping loop. The UDP monitor tracks confirmed local queue overflows (NIC sysfs drop counters, `/proc/net/snmp` UDP buffer errors, and `AF_PACKET` `PACKET_STATISTICS` ring drops) as well as per-flow directional delivery gaps and arrival-timing/burst heuristics indicative of packet reordering or queueing stalls, reporting unified chronological alerts and a combined end-of-session summary.

## Technical Context

**Language/Version**: Rust 1.99.1 (2021 edition)  
**Primary Dependencies**: `etherparse = "0.21"`, `libc = "0.2"`, Rust standard library (`std::thread`, `std::net`, `std::collections`, `std::fs`, `std::time`)  
**Storage**: N/A (in-memory per-session state and real-time console output)  
**Testing**: `cargo test` for pure unit tests (flow state machine, heuristic detectors, counter parsers, CLI/discovery logic) plus root-privileged live/loopback validation runs  
**Target Platform**: Linux (Ubuntu 24.04, `AF_PACKET`, `/proc/net/snmp`, `/sys/class/net/<iface>/statistics/`)  
**Project Type**: Single-binary CLI diagnostic utility  
**Performance Goals**: Process live gameplay UDP packet rates (typically 30–1,000+ pps per flow) with sub-second alert latency (`< 1s`), minimal CPU overhead, and negligible `AF_PACKET` capture ring drops  
**Constraints**: Raw sockets require `CAP_NET_RAW` / `sudo`; preserve existing 5 positional CLI arguments without breaking current invocations; keep ICMP ping timing unaffected by UDP packet bursts; avoid false positives on bidirectional traffic, bridge duplicates, and idle pauses  
**Scale/Scope**: Single active target server IPv4 per run with 1 to dozens of associated UDP flows/ports and bounded per-flow O(1) memory state

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

`.specify/memory/constitution.md` is currently an unpopulated template with no project-specific ratified rules. Evaluating standard engineering gates for this repository:

- **Single-Binary Simplicity**: Keep existing Rust binary architecture without adding heavy async runtimes or external daemon dependencies. **PASS**
- **Backward Compatibility**: Preserve existing positional CLI syntax (`[secs] [interval_ms] [spike_ms] [port|any] [iface]`) and existing `[SPIKE]`/`[TIMEOUT]` outputs. **PASS**
- **Testability Without Root**: Isolate pure UDP flow tracking, timing heuristics, and Linux counter parsing from raw socket FFI so `cargo test` runs unprivileged in CI/local dev. **PASS**
- **Minimal Dependency Footprint**: Implement all new behavior using existing `etherparse`, `libc`, and `std` without adding new Cargo crates. **PASS**

Post-Phase 1 Re-Check: All four gates remain **PASS** after Phase 1 design (`data-model.md`, `contracts/cli-output.md`, `quickstart.md`).

## Project Structure

### Documentation (this feature)

```text
specs/001-udp-packet-monitoring/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
│   └── cli-output.md    # CLI behavior, log event tags, and summary format contract
└── tasks.md             # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
Cargo.toml
src/
├── main.rs              # CLI parsing, signal handling, discovery orchestration, concurrent ICMP + UDP launch
├── discovery.rs         # Packet discovery ranking and associated UDP port collection (extracted/tested)
├── ping.rs              # Continuous ICMP Echo Request/Reply loop (preserved behavior)
├── udp_monitor.rs       # AF_PACKET UDP capture loop, directional flow state machine, gap & burst heuristics
└── queue_counters.rs    # Linux NIC sysfs, /proc/net/snmp, and PACKET_STATISTICS drop delta tracking
```

**Structure Decision**: Single-project Rust crate layout under `src/`, splitting `src/main.rs` into focused modules (`discovery.rs`, `ping.rs`, `udp_monitor.rs`, `queue_counters.rs`) while keeping a single compiled binary (`game_ping_watch`). Pure parsing and state-machine logic lives in testable functions with inline `#[cfg(test)]` unit tests so `cargo test` validates behavior without requiring root sockets.

## Complexity Tracking

No constitution violations or architectural exceptions required.
