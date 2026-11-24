# Changelog

All notable changes to `game_ping_watch` are documented in this file.

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
