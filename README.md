# game_ping_watch

`game_ping_watch` discovers an active remote game server IP from live network traffic and runs a continuous ICMP ping loop until you press `Ctrl+C`. It logs round-trip time (RTT) spikes and timeouts to help spot network lag during gameplay.

## How It Works

1. **Discovery phase:** Captures raw Ethernet frames (`0x0800` IPv4 and `0x8100` 802.1Q VLAN) on `AF_PACKET` for a set time window (default `60` seconds). It skips local, multicast, broadcast, link-local, and DNS/mDNS/SSDP traffic, ranks public IPv4 endpoints by byte count, and selects the top address.
2. **Ping phase:** Opens a raw ICMP socket (`SOCK_RAW`, `IPPROTO_ICMP`) and sends Echo Requests at a fixed interval (default `200` ms). It prints `[SPIKE]` when RTT or RTT change reaches the spike threshold, prints `[TIMEOUT]` when a reply does not arrive within `1000` ms, and prints a summary when you press `Ctrl+C`.

## Build

Requires Rust and `cargo`.

```bash
cd ~/projects/game_ping_watch
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