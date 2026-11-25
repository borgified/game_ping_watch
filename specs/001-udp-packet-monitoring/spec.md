# Feature Specification: UDP Packet Drop and Order Monitoring

**Feature Branch**: `001-udp-packet-monitoring`

**Created**: 2026-11-25

**Status**: Draft

**Input**: User description: "the first half of the app already detects the likely game server ip by counting packets. with this ip, we run a ping test but this alone is not enough to verify laggy gameplay. we should also monitor UDP packets for queue drops or out of order delivery on those ports associated by the detected ip."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Track All UDP Ports Associated With the Detected Server IP (Priority: P1)

During the server discovery phase, the application identifies the most active public game server IP address. Alongside selecting that IP address, it collects the UDP ports associated with traffic to and from that server IP so that post-discovery monitoring covers all relevant game communication channels automatically.

**Why this priority**: Monitoring UDP health requires knowing which ports carry traffic for the selected server IP. Without accurate port association, UDP monitoring either misses game traffic or triggers noise from unrelated background applications.

**Independent Test**: Run discovery while generating UDP traffic across multiple ports toward a single public server IP, and verify that all active UDP ports for that IP are captured and selected for monitoring while unrelated destinations are excluded.

**Acceptance Scenarios**:

1. **Given** active UDP traffic between the user's machine and a remote game server IP across two different UDP ports during discovery, **When** discovery completes and selects that server IP, **Then** both UDP ports are listed as associated with the detected server IP and handed off to the monitoring phase.
2. **Given** background UDP traffic on the same port numbers going to a different public IP address, **When** the monitoring phase begins for the detected server IP, **Then** traffic to other IP addresses is ignored.

---

### User Story 2 - Detect UDP Queue Drops During Gameplay (Priority: P1)

While the continuous ping test runs against the detected server IP, the application concurrently monitors both local queue drop counters and UDP flow delivery gaps on the server's associated ports. Because ICMP ping replies can remain healthy even when game packets are dropped in local buffers or stalled upstream, real-time alerts and drop counts let the user confirm whether queue drops occurred during laggy moments.

**Why this priority**: Packet drops directly cause rubber-banding and lost player actions in real-time games. Detecting both local buffer/interface drops and flow-level delivery gaps alongside latency spikes is the primary reason for extending beyond ping-only monitoring.

**Independent Test**: Run the monitoring phase against a target server IP and associated UDP ports while inducing local queue drops or flow delivery gaps, and verify that timestamped alerts appear in real time and match the session summary totals.

**Acceptance Scenarios**:

1. **Given** an active monitoring session for a detected server IP and its associated UDP ports, **When** a local interface, host UDP buffer, or capture queue drop increments during monitoring, **Then** the tool outputs a timestamped queue drop alert reporting the drop source and delta count.
2. **Given** an active steady-rate UDP flow on an associated port, **When** an unexpected mid-flow delivery gap or observable sequence gap occurs, **Then** the tool outputs a timestamped flow-gap alert identifying the affected port and direction.
3. **Given** a monitoring session in which queue drops or flow gaps occur alongside normal ping responses, **When** the user ends the session, **Then** the final summary displays both the ping metrics and separate totals for local queue drops and flow-gap events.

---

### User Story 3 - Detect Out-of-Order / Bursty UDP Delivery via Timing Heuristics (Priority: P2)

While monitoring the detected server IP's associated UDP ports, the application analyzes directional packet arrival timing and burst patterns on each flow. Because standard game UDP headers lack universal sequence numbers, the tool uses arrival-timing and burst heuristics (such as abnormal inter-packet stalls followed by compressed back-to-back packet bursts) to flag likely packet reordering or upstream queueing jitter without requiring proprietary payload decoding.

**Why this priority**: Queued-then-flushed or reordered packet bursts degrade real-time game state synchronization even when average ping latency and loss look acceptable.

**Independent Test**: Replay or inject a steady UDP stream on a monitored port for the detected IP with an artificial stall followed by a compressed burst of packets, and verify that the heuristic anomaly is flagged with a timestamp and counted in the summary while steady bidirectional traffic produces zero alerts.

**Acceptance Scenarios**:

1. **Given** an active UDP flow with an established packet cadence between the user's machine and the detected server IP, **When** packets within a single direction exhibit an abnormal arrival stall followed by a tight burst or severe inter-arrival jitter jump, **Then** the tool logs a timestamped delivery-order/burst heuristic alert with the flow port and direction.
2. **Given** normal bidirectional UDP gameplay where inbound and outbound packets interleave independently at steady cadence, **When** each direction maintains normal spacing, **Then** zero heuristic reordering alerts are triggered.

---

### User Story 4 - Correlated Real-Time Output and Session Summary (Priority: P2)

The user views ping events (latency spikes and timeouts) and UDP anomalies (confirmed local queue drops, flow gaps, and heuristic burst/reordering alerts) in a single chronological output stream and combined end-of-session summary. This lets the user immediately see whether a lag episode affected both ICMP and UDP, only UDP, or only ICMP.

**Why this priority**: Correlating ping behavior with UDP anomalies in one timeline avoids needing separate tools or manual log alignment to diagnose gameplay lag.

**Independent Test**: Execute a monitoring session containing both ping spikes/timeouts and UDP anomalies, stop the session, and verify that live events appear in chronological order and the end-of-session summary presents all metrics clearly.

**Acceptance Scenarios**:

1. **Given** a running session monitoring both ping and UDP traffic for the detected server IP, **When** a ping spike and a UDP queue drop occur within the same second, **Then** both alerts are printed with consistent timestamps in chronological order.
2. **Given** the user stops the session, **When** the summary prints, **Then** it includes total ping metrics alongside total monitored UDP packets, monitored ports, local queue drops, flow-gap events, and heuristic burst/order alerts.

---

### Edge Cases

- What happens when the detected server IP exchanges no UDP traffic during discovery (for example, TCP-only traffic)? The tool continues running the ping test, reports that no UDP ports were observed yet, and monitors UDP if traffic to that IP starts later.
- What happens when the game server opens a new UDP port with the detected IP after the discovery window closes? The monitor detects and includes new UDP flows involving the detected server IP automatically rather than silently missing mid-match port switches.
- What happens when UDP gameplay traffic pauses naturally (between matches, loading screens, or while idle in menus)? Extended idle silence or low-rate keepalive traffic must not continuously trigger flow-gap or burst alerts once a flow enters an idle state.
- What happens when the user's machine exchanges UDP traffic with other servers using the same port numbers? Only packets whose remote endpoint matches the detected server IP are included in flow-level UDP monitoring, while host/interface queue drop counters report system/interface-level drops clearly labeled as host or interface scope.
- What happens when a server drops or rate-limits ICMP ping replies while UDP game traffic continues normally? UDP packet monitoring and alerting continue uninterrupted regardless of ping timeouts.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST record all UDP ports observed exchanging packets with the selected server IP address during the discovery phase.
- **FR-002**: System MUST monitor UDP traffic exchanged with the selected server IP concurrently with the post-discovery ping loop.
- **FR-003**: System MUST monitor all UDP ports associated with the selected server IP from discovery and dynamically include additional UDP ports observed communicating with that same server IP during the monitoring phase.
- **FR-004**: System MUST filter out flow-level UDP traffic belonging to any remote IP address other than the selected server IP, even when port numbers match.
- **FR-005**: System MUST detect and count both (a) local queue overflows (deltas in network interface RX/TX drops, host UDP receive buffer errors, and packet-capture queue drops during the session) and (b) flow-level delivery gaps on active monitored UDP flows (detected via unexpected mid-flow packet silence relative to recent active flow cadence, or observable monotonic sequence gaps when available).
- **FR-006**: System MUST detect and count suspected out-of-order or queued-burst UDP delivery per monitored directional flow using arrival-timing and burst heuristics (identifying abnormal inter-arrival jitter spikes or silence-followed-by-burst packet clusters relative to the flow's baseline cadence), and MUST label these alerts as heuristic timing/burst detections rather than proven sequence swaps.
- **FR-007**: System MUST track inbound and outbound UDP flows separately per port pair so interleaved bidirectional traffic is never misclassified as a timing/burst anomaly.
- **FR-008**: System MUST emit a timestamped real-time alert whenever a local queue drop, flow-gap event, or heuristic burst/order anomaly occurs, clearly identifying the event category, source/port, and direction (where applicable), using the same timestamp format as ping alerts.
- **FR-009**: System MUST present an end-of-session summary that adds UDP monitoring totals (monitored ports, UDP packets observed, local queue drop counts by scope, flow-gap count, and heuristic burst/order alert count) alongside the existing ping summary.
- **FR-010**: System MUST continue operating normally if no UDP ports are associated with the selected server IP, notifying the user that UDP monitoring has no active ports yet while remaining ready if UDP traffic to that IP starts later.

### Key Entities *(include if feature involves data)*

- **Detected Server Target**: The top-ranked public IPv4 server address selected by discovery together with its set of associated UDP ports.
- **Monitored UDP Flow**: A directional stream of UDP packets between the local host and the detected server IP, identified by local port, remote port, and direction (inbound vs. outbound), maintaining recent inter-arrival baseline statistics.
- **Local Queue Counter Snapshot**: Periodic baseline and delta measurements of interface-level, host UDP buffer-level, and capture-ring drop counts during the session.
- **UDP Anomaly Event**: A timestamped diagnostic record categorized as a local queue drop, a flow-level delivery gap, or a heuristic burst/reordering event.
- **Session Diagnostic Summary**: Combined end-of-run statistics covering ICMP ping health, local queue drops, and per-session UDP flow health for the detected server IP.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 100% of UDP ports actively communicating with the selected server IP during discovery are automatically tracked in the subsequent monitoring phase without manual port re-entry.
- **SC-002**: Real-time alerts for detected local queue drops, flow gaps, and heuristic burst/order anomalies appear in the output within 1 second of detection under active gameplay packet rates.
- **SC-003**: Steady-state bidirectional UDP game traffic with normal spacing produces 0% false-positive heuristic burst/order or flow-gap alerts.
- **SC-004**: Users can determine from a single session summary whether lag coincided with ICMP latency anomalies, local host/interface queue drops, UDP flow gaps/bursts, or a combination of these.

## Assumptions

- The existing server discovery and continuous ICMP ping loop remain the core workflow, with UDP monitoring added concurrently during the ping phase rather than as a separate manual command.
- Because arrival-timing and burst heuristics infer likely reordering and queueing stalls without decoding encrypted or proprietary game payloads, heuristic thresholds use a brief per-flow warm-up window to establish baseline packet cadence and treat prolonged silence as idle state rather than continuous packet drops.
- Standard excluded background protocols from discovery (such as DNS, mDNS, and SSDP) remain excluded unless explicitly targeted, and non-game traffic to other IP addresses is excluded from flow-level UDP analysis.
- Real-time console output and `Ctrl+C` session termination remain the primary user interaction model.
- Elevated network capture privileges already required by the existing application continue to be available during the monitoring phase.
