# Changelog

All notable changes to `game_ping_watch` are documented in this file.

## [0.2.0] - 2026-11-25

### Added
- Multi-port UDP association during discovery (`src/discovery.rs`) and dynamic post-discovery `[UDP-PORT]` tracking for the selected server IP (`src/udp_monitor.rs`).
- Concurrent `AF_PACKET` UDP monitoring thread alongside ICMP `ping_loop()` with mutex-serialized timestamped output (`src/main.rs`, `src/ping.rs`).
- Linux local queue drop delta monitoring across NIC sysfs counters, `/proc/net/snmp` UDP errors, and `AF_PACKET` `PACKET_STATISTICS` (`[UDP-DROP]` in `src/queue_counters.rs`).
- Per-flow directional cadence gap (`[UDP-GAP]`) and arrival-timing burst heuristics (`[UDP-BURST] heuristic=order/burst`) with combined end-of-session `UDP <ip>:` summary (`src/udp_monitor.rs`).
- Unprivileged unit test coverage (`13` tests) across `discovery`, `queue_counters`, `udp_monitor`, and CLI port parsing.
- Spec-kit documentation artifacts under `specs/001-udp-packet-monitoring/` and updated `README.md` / `AGENT_HANDOFF.md`.

### Changed
- Refactored monolithic `src/main.rs` into focused modules (`discovery.rs`, `ping.rs`, `queue_counters.rs`, `udp_monitor.rs`) and updated `.gitignore` for Rust build artifacts.

## [0.1.0] - 2026-11-24

### Added
- Raw `AF_PACKET` server IP discovery ranked by public IPv4 byte count (`src/main.rs`).
- 802.1Q VLAN (`0x8100`) frame unwrapping in `discover_ip()` alongside plain IPv4 Ethernet (`0x0800`) frames.
- Optional 5th CLI argument (`iface`) bound to `AF_PACKET` via `if_nametoindex` and `sockaddr_ll`.
- `-h` / `--help` CLI usage output.
- Continuous raw ICMP Echo Request/Reply monitoring loop with `[SPIKE]`, `[TIMEOUT]`, and `Ctrl+C` summary output.
- Project documentation (`README.md`, `AGENT_HANDOFF.md`).

### Fixed
- Fixed `main()` port argument parsing so `"any"` and `"*"` resolve to `None` while omitted or empty arguments default to `Some(32862)`.
