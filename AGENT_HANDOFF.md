# game_ping_watch Handoff

- Goal: Diagnose intermittent rubber-banding lag in World of Tanks Blitz on Ubuntu 24.04 by discovering the active game server IP via raw packet capture and running continuous ICMP RTT monitoring (`[SPIKE]` and `[TIMEOUT]`).
- Current code: ~/projects/game_ping_watch/src/main.rs and Cargo.toml.
- What is verified healthy: `ip -s link show` on `enp37s0` and `br0` shows active traffic and zero drops/errors. Raw socket privileges work via `sudo`.
- Remaining bugs to fix & verify:
  1. In `main()`, passing `any` still resulted in `port=Some(32862)` because `.filter(...)` -> `None` was followed by `.or(Some(32862))`. Fix with an explicit `match` so `"any"` or `"*"` is `None` and only a missing arg defaults to `Some(32862)`.
  2. In `discover_ip()`, unwrap 802.1Q VLAN frames (`0x8100` -> inner `0x0800` at offset 18) in addition to plain Ethernet (`0x0800` at offset 14).
  3. Support an optional 5th CLI argument for interface name (e.g., `br0` or `enp37s0`) bound on `AF_PACKET`.
  4. Build with `cargo build --release` and test `sudo ./target/release/game_ping_watch 20 200 80 any` during gameplay.
