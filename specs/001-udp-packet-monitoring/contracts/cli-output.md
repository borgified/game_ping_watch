# Interface Contract: CLI & Diagnostic Output

**Feature**: `001-udp-packet-monitoring` | **Date**: 2026-11-25

## 1. CLI Invocation Contract

Preserve exact positional CLI argument compatibility with `v0.1.0`:

```text
game_ping_watch [secs=60] [interval_ms=200] [spike_ms=80] [port=32862|any] [iface]
```

- All existing positional arguments retain identical defaults, parsing rules, and `-h` / `--help` support.
- No new required flags or positional arguments are added; UDP monitoring activates automatically after discovery selects a target IPv4 address.

## 2. Discovery Output Contract (`stderr`)

Discovery prints the top IPv4 candidates table and adds associated UDP port visibility:

```text
Top IPv4 candidates:
  <ip>:<port> pkts=<count> bytes=<bytes>
Selected target: <ip> (associated UDP ports: <p1, p2, ... | none>)
```

- If `associated UDP ports` is `none` (for example, TCP-only discovery), the monitor informs the user that no UDP ports were observed yet and remains ready to track UDP ports if traffic to `<ip>` begins during monitoring.

## 3. Monitoring Startup & Real-Time Event Contract (`stdout`)

Immediately before starting the concurrent loops:

```text
Pinging <ip> every <interval_ms>ms (spike >= <spike_ms>ms, timeout=<timeout_ms>ms).
Monitoring UDP for <ip> on ports [<p1, p2, ... | waiting for UDP>]. Ctrl+C to stop.
```

Real-time event lines use the existing `HH:MM:SS.ffffff` timestamp format (`ts_now()`), emitted chronologically:

| Event Tag | Trigger | Format |
|---|---|---|
| `[SPIKE]` | Existing ICMP RTT/delta threshold exceeded | `[SPIKE] <ts> seq=<seq> rtt=<ms>ms delta=<ms>ms` |
| `[TIMEOUT]` | Existing ICMP reply timeout exceeded | `[TIMEOUT] <ts> seq=<seq> > <timeout_ms>ms` |
| `[UDP-PORT]` | New remote UDP port observed for `<ip>` post-discovery | `[UDP-PORT] <ts> port=<rport> (local=<lport>, <dir>)` |
| `[UDP-DROP]` | Confirmed local NIC, host UDP buffer, or capture ring drop delta | `[UDP-DROP] <ts> scope=<nic:<if>\|host:udp\|capture:af_packet> metric=<name> +<delta>` |
| `[UDP-GAP]` | Mid-flow delivery silence exceeding active cadence threshold | `[UDP-GAP] <ts> port=<rport> dir=<in\|out> gap=<ms>ms (baseline=<ms>ms)` |
| `[UDP-BURST]` | Heuristic burst/reordering anomaly (labeled heuristic) | `[UDP-BURST] <ts> port=<rport> dir=<in\|out> heuristic=order/burst pkts=<n> gap=<ms>ms min_iat=<ms>ms` |

**Contract Guarantees**:
- `[UDP-BURST]` MUST explicitly indicate `heuristic=order/burst` so heuristic timing alerts are never mistaken for confirmed UDP payload sequence swaps.
- `[UDP-DROP]` MUST state its exact `scope` (`nic:<iface>`, `host:udp`, or `capture:af_packet`) so system-wide host/NIC drops remain distinguishable from flow-specific `[UDP-GAP]` events.

## 4. End-of-Session Summary Contract (`stdout`)

Printed upon `SIGINT` (`Ctrl+C`) after both the ICMP loop and UDP monitor thread join cleanly:

```text
Summary <ip>: sent=<n> recv=<n> timeouts=<n> loss=<pct>% spikes=<n>
RTT ms: min=<ms> avg=<ms> max=<ms>
UDP <ip>: ports=[<p1,p2,...|none>] pkts=<total> (in=<in>, out=<out>) local_drops(nic=<n>, host=<n>, cap=<n>) flow_gaps=<n> heuristic_bursts=<n>
```

- Existing ICMP `Summary` and `RTT ms` lines retain their exact format for scripts/readers expecting them.
- The `UDP <ip>:` line appends UDP port coverage, directional packet counts, confirmed local queue drop totals by scope, flow-gap count, and heuristic burst/order count.
