# Quickstart & Validation Guide: UDP Packet Drop and Order Monitoring

**Feature**: `001-udp-packet-monitoring` | **Date**: 2026-11-25

## Prerequisites

- Linux host (Ubuntu 24.04 or compatible kernel exposing `/proc/net/snmp` and `/sys/class/net/`)
- Rust toolchain (`rustc` and `cargo`)
- Root or `CAP_NET_RAW` capability (`sudo`) for live packet capture and ICMP ping validation

## 1. Build & Unprivileged Unit Verification

Run the unprivileged build and unit test suite to verify discovery port tracking, directional flow separation, gap/burst heuristics, and Linux counter parsing:

```bash
cd /home/fwiffo/projects/game_ping_watch
cargo test
cargo build --release
```

**Expected Outcome**: All unit tests in `discovery`, `udp_monitor`, and `queue_counters` pass without root privileges, and `./target/release/game_ping_watch` builds cleanly.

## 2. Live Gameplay / Traffic Validation (`sudo`)

Run against active game traffic (or across all ports on a specific interface such as `br0` or `enp37s0`):

```bash
sudo ./target/release/game_ping_watch 15 200 80 any br0
```

**Expected Outcome**:
1. Discovery lists top IPv4 candidates and displays `Selected target: <ip> (associated UDP ports: ...)`.
2. Monitoring starts with both `Pinging <ip>...` and `Monitoring UDP for <ip> on ports [...]`.
3. Normal steady bidirectional gameplay traffic produces zero `[UDP-BURST]` or `[UDP-GAP]` false positives.
4. Pressing `Ctrl+C` prints both the ICMP `Summary` / `RTT ms` lines and the `UDP <ip>:` summary line.

## 3. Simulated Anomaly Validation Scenarios

Verify each anomaly path in a controlled test environment:

1. **Multi-Port & Dynamic Port Discovery (User Story 1)**:
   - Send UDP traffic to a single public test IP on two ports during the discovery window, then start sending on a third UDP port to that same IP during monitoring.
   - Verify the first two ports appear at startup and the third triggers a `[UDP-PORT]` line and appears in the final `UDP <ip>: ports=[...]` list.
2. **Flow Gap & Heuristic Burst Detection (User Stories 2 & 3)**:
   - Stream steady UDP packets (e.g., every `20ms`) toward a monitored remote IP, pause transmission for `300ms`, then immediately send a compressed burst of `5` packets (`< 1ms` apart).
   - Verify `[UDP-GAP]` fires for the mid-flow silence and `[UDP-BURST] ... heuristic=order/burst` fires on burst recovery, without triggering false alerts when steady `20ms` bidirectional traffic resumes.
3. **Local Queue Drop Correlation (User Story 2)**:
   - During an active monitoring run, induce or observe interface/UDP buffer drops (or verify counter delta polling via unit fixtures for `/proc/net/snmp` and `/sys/class/net/<iface>/statistics/`).
   - Verify `[UDP-DROP]` prints with exact `scope=` and `+delta`, and totals appear under `local_drops(...)` in the end-of-session summary.
4. **TCP-Only / Zero-UDP Fallback (Edge Case)**:
   - Run discovery against a TCP-only public endpoint (for example, HTTPS download on port `443` with discovery port `any`).
   - Verify the ICMP ping loop proceeds normally and UDP monitoring reports `ports=[none]` without crashing or emitting spurious drop alerts.
