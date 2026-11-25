# game_ping_watch Handoff

- Goal: Diagnose intermittent rubber-banding lag in World of Tanks Blitz on Ubuntu 24.04 by discovering the active game server IP and its associated UDP ports via raw packet capture, then concurrently monitoring ICMP RTT (`[SPIKE]`, `[TIMEOUT]`) and UDP health (`[UDP-PORT]`, `[UDP-DROP]`, `[UDP-GAP]`, `[UDP-BURST]`).
- Current code layout:
  - `src/main.rs` — CLI parsing (`[secs=60] [interval_ms=200] [spike_ms=80] [port=32862|any] [iface]`), `SIGINT` handling, atomic `log_line()` output, and concurrent ICMP + UDP orchestration.
  - `src/discovery.rs` — Raw `AF_PACKET` discovery with 802.1Q VLAN unwrapping and multi-port UDP association per public IPv4 candidate.
  - `src/ping.rs` — Continuous raw ICMP Echo Request/Reply loop.
  - `src/queue_counters.rs` — Linux `/sys/class/net/<iface>/statistics/`, `/proc/net/snmp` (`Udp:`), and `AF_PACKET` `PACKET_STATISTICS` drop delta tracking.
  - `src/udp_monitor.rs` — Concurrent `AF_PACKET` UDP monitor with directional flow separation (`ifindex`, `local_port`, `remote_port`, `direction`), dynamic port discovery, cadence-gap alerts, and arrival-timing burst heuristics.
- What is verified:
  - `ip -s link show` on `enp37s0` and `br0` shows active traffic and zero drops/errors. Raw socket privileges work via `sudo`.
  - Unprivileged `cargo test` passes (`13` unit tests) and `cargo build --release` builds cleanly.
- Remaining live verification:
  - Build with `cargo build --release` and run `sudo ./target/release/game_ping_watch 20 200 80 any` (or specify `br0` / `enp37s0`) during live gameplay to validate live alerts and combined `Ctrl+C` summary output.
