# game_ping_watch

`game_ping_watch` discovers an active remote game server IP from live network traffic, runs a continuous ICMP ping loop, monitors UDP traffic on all ports associated with that IP, and runs periodic `mtr` path cycles until you press `Ctrl+C`. It logs round-trip time (RTT) spikes, ping timeouts, local UDP/NIC/capture queue drops, mid-flow UDP silence gaps, arrival-timing burst heuristics, and per-hop `mtr` path anomalies to help diagnose lag during gameplay.

## How It Works

1. **Discovery phase:** Captures raw Ethernet frames (`0x0800` IPv4 and `0x8100` 802.1Q VLAN) on `AF_PACKET` for a set time window (default `60` seconds). It skips local, multicast, broadcast, link-local, and DNS/mDNS/SSDP traffic (unless explicitly filtered), ranks public IPv4 endpoints by byte count, selects the top address, and records all associated UDP ports for that IP.
2. **Ping, UDP, and MTR monitoring phase:** Runs a continuous raw ICMP Echo loop alongside a concurrent `AF_PACKET` UDP monitor and periodic `mtr -r -w -n -c 5` cycles for the selected server IP. It logs `[SPIKE]` and `[TIMEOUT]` for ICMP, `[UDP-PORT]` for newly discovered ports, `[UDP-DROP]` for local NIC/host/capture queue overflows, `[UDP-GAP]` for mid-flow silence stalls, `[UDP-BURST]` (`heuristic=order/burst`) for arrival-timing burst clusters, and `[MTR]` / `[MTR-HOP]` for per-cycle path windows and hop loss/latency spikes. Pressing `Ctrl+C` prints combined ICMP, UDP, and hop-by-hop MTR summaries.

## Build

Requires Rust and `cargo`.

```bash
cd ~/projects/game_ping_watch
cargo test
cargo build --release
```

## Usage

Raw packet capture and raw ICMP sockets require root privileges (`sudo`).

```bash
sudo ./target/release/game_ping_watch [discover_secs] [interval_ms] [spike_threshold_ms] [udp_port] [interface]
./target/release/game_ping_watch --help
```

### Arguments

* `discover_secs` (default `60`): Discovery capture duration in seconds.
* `interval_ms` (default `200`): Time between ICMP ping packets in milliseconds.
* `spike_threshold_ms` (default `80.0`): RTT or RTT jump threshold in milliseconds that triggers a `[SPIKE]` line.
* `udp_port` (default `32862`): Filter traffic to this UDP/TCP port during discovery. Use `any` or `*` to check all non-excluded ports.
* `interface` (optional): Target network interface name such as `br0` or `enp37s0`, bound on `AF_PACKET`.

### Examples

Default run for World of Tanks Blitz traffic on UDP port `32862`:

```bash
sudo ./target/release/game_ping_watch 60 200 80 32862
```

Short `10`-second discovery test with a `40` ms spike threshold:

```bash
sudo ./target/release/game_ping_watch 10 200 40 32862
```

Discover across all ports for `30` seconds:

```bash
sudo ./target/release/game_ping_watch 30 200 80 any
```

Discover across all ports on `br0` for `20` seconds:

```bash
sudo ./target/release/game_ping_watch 20 200 80 any br0
```

## Output Notes

* **No candidate found:** Start the game or generate traffic on the target port during the discovery window, or pass `any` for `udp_port`.
* **Bridge and NIC traffic checks (`br0` / `enp37s0`):** If your setup uses a bridge (`br0`) and physical interface (`enp37s0`), check that packets and bytes are moving on those interfaces with zero drops before or during discovery:

```bash
ip -s link show enp37s0
ip -s link show br0
```

* **ICMP filtering:** Some game servers or transit hops drop or rate-limit ICMP Echo Replies. Zero replies or high timeouts can mean ICMP filtering rather than full game packet loss.
* **UDP diagnostic alerts:**
  * `[UDP-PORT]`: A new remote UDP port started communicating with the detected server IP after discovery ended.
  * `[UDP-DROP]`: Confirmed local drop delta on a NIC (`nic:<iface>`), kernel UDP buffer (`/proc/net/snmp`, `host:udp`), or `AF_PACKET` ring (`capture:af_packet`).
  * `[UDP-GAP]`: Mid-flow packet silence on an active directional UDP stream exceeding its baseline cadence threshold.
  * `[UDP-BURST] ... heuristic=order/burst`: Arrival-timing burst cluster (such as a stall followed by tightly bunched packets) flagged heuristically without decoding encrypted game payloads.
* **Combined summary:** Pressing `Ctrl+C` prints ICMP `Summary` and `RTT ms` lines, `UDP <ip>: ports=[...] pkts=... local_drops(nic=..., host=..., cap=...) flow_gaps=... heuristic_bursts=...`, and `MTR <ip>: cycles=...` with per-hop cumulative stats.
* **MTR diagnostic alerts:**
  * `[MTR]`: Timestamped window (`window=start..end`) and target hop summary after each `mtr -r -w -n -c 5` cycle.
  * `[MTR-HOP]`: Hop-level packet loss (excluding persistently silent `???` hops) or latency spike (`wrst >= spike_threshold_ms`).