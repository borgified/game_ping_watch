use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Read};
use std::net::Ipv4Addr;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use crate::{log_line, ts_now, STOP};

const MTR_PROBES_PER_CYCLE: u32 = 5;
const CHILD_POLL_MS: u64 = 100;
const ERROR_RETRY_MS: u64 = 1000;

/// Parsed statistics for a single hop in one `mtr -r -w -n` cycle.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MtrHopSample {
    pub hop: u8,
    pub host: String,
    pub loss_pct: f64,
    pub snt: u64,
    pub last_ms: f64,
    pub avg_ms: f64,
    pub best_ms: f64,
    pub wrst_ms: f64,
    pub stdev_ms: f64,
}

/// Hop-level loss or latency spike detected within an `mtr` cycle.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MtrHopAlert {
    pub hop: u8,
    pub host: String,
    pub loss_pct: f64,
    pub last_ms: f64,
    pub avg_ms: f64,
    pub best_ms: f64,
    pub wrst_ms: f64,
    pub stdev_ms: f64,
}

/// Summary extracted for a single completed `mtr` cycle.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MtrCycleOutcome {
    pub cycle: u64,
    pub hop_count: usize,
    pub target_hop: Option<MtrHopSample>,
    pub alerts: Vec<MtrHopAlert>,
}

/// Aggregated statistics for one hop across all completed `mtr` cycles.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MtrHopAggregate {
    pub hop: u8,
    pub host: String,
    pub ever_responded: bool,
    pub total_snt: u64,
    total_lost_est: f64,
    total_recv_est: f64,
    avg_weighted_sum: f64,
    pub best_ms: Option<f64>,
    pub wrst_ms: f64,
    pub last_ms: f64,
}

impl MtrHopAggregate {
    fn new(hop: u8) -> Self {
        Self {
            hop,
            host: "???".to_string(),
            ever_responded: false,
            total_snt: 0,
            total_lost_est: 0.0,
            total_recv_est: 0.0,
            avg_weighted_sum: 0.0,
            best_ms: None,
            wrst_ms: 0.0,
            last_ms: 0.0,
        }
    }

    pub(crate) fn loss_pct(&self) -> f64 {
        if self.total_snt == 0 {
            0.0
        } else {
            ((self.total_lost_est / self.total_snt as f64) * 100.0).clamp(0.0, 100.0)
        }
    }

    pub(crate) fn avg_ms(&self) -> f64 {
        if self.total_recv_est > 0.0 {
            self.avg_weighted_sum / self.total_recv_est
        } else {
            0.0
        }
    }

    pub(crate) fn best_ms_or_zero(&self) -> f64 {
        self.best_ms.unwrap_or(0.0)
    }
}

/// End-of-session aggregated `mtr` summary across all completed cycles.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MtrSummary {
    pub cycles: u64,
    pub probes_per_cycle: u32,
    pub unavailable_reason: Option<String>,
    pub hops: Vec<MtrHopAggregate>,
}

/// Pure stateful tracker aggregating `mtr` cycles and flagging hop anomalies.
pub(crate) struct MtrTracker {
    target_ip_str: String,
    spike_ms: f64,
    cycles: u64,
    probes_per_cycle: u32,
    unavailable_reason: Option<String>,
    hops: BTreeMap<u8, MtrHopAggregate>,
}

impl MtrTracker {
    pub(crate) fn new(target_ip: Ipv4Addr, spike_ms: f64, probes_per_cycle: u32) -> Self {
        Self {
            target_ip_str: target_ip.to_string(),
            spike_ms,
            cycles: 0,
            probes_per_cycle,
            unavailable_reason: None,
            hops: BTreeMap::new(),
        }
    }

    pub(crate) fn set_unavailable(&mut self, reason: String) {
        self.unavailable_reason = Some(reason);
    }

    /// Record one parsed `mtr` cycle, updating aggregates and returning alerts.
    pub(crate) fn record_cycle(&mut self, samples: &[MtrHopSample]) -> MtrCycleOutcome {
        self.cycles += 1;

        let target_hop_num = samples
            .iter()
            .find(|s| s.host == self.target_ip_str)
            .or_else(|| samples.last())
            .map(|s| s.hop);

        let mut alerts = Vec::new();
        let mut target_sample = None;

        for s in samples {
            if Some(s.hop) == target_hop_num {
                target_sample = Some(s.clone());
            }

            let agg = self
                .hops
                .entry(s.hop)
                .or_insert_with(|| MtrHopAggregate::new(s.hop));

            let previously_responded = agg.ever_responded;
            let prev_best = agg.best_ms;

            let responded_this_cycle = s.host != "???" && s.loss_pct < 100.0;
            if s.host != "???" {
                agg.host = s.host.clone();
            }
            if responded_this_cycle {
                agg.ever_responded = true;
            }

            let snt_f = s.snt as f64;
            let lost_f = snt_f * (s.loss_pct / 100.0);
            let recv_f = (snt_f - lost_f).max(0.0);
            agg.total_snt += s.snt;
            agg.total_lost_est += lost_f;
            if responded_this_cycle && recv_f > 0.0 {
                agg.total_recv_est += recv_f;
                agg.avg_weighted_sum += s.avg_ms * recv_f;
                agg.best_ms = Some(agg.best_ms.map_or(s.best_ms, |b| b.min(s.best_ms)));
                agg.wrst_ms = agg.wrst_ms.max(s.wrst_ms);
                agg.last_ms = s.last_ms;
            }

            let is_target_hop = Some(s.hop) == target_hop_num;
            let has_notable_loss = if s.loss_pct <= 0.0 {
                false
            } else if s.loss_pct < 100.0 {
                s.host != "???"
            } else {
                is_target_hop || previously_responded
            };

            let baseline_best = prev_best.unwrap_or(s.best_ms);
            let has_latency_spike = responded_this_cycle
                && (s.wrst_ms >= self.spike_ms
                    || (baseline_best > 0.0 && (s.wrst_ms - baseline_best) >= self.spike_ms));

            if has_notable_loss || has_latency_spike {
                alerts.push(MtrHopAlert {
                    hop: s.hop,
                    host: s.host.clone(),
                    loss_pct: s.loss_pct,
                    last_ms: s.last_ms,
                    avg_ms: s.avg_ms,
                    best_ms: s.best_ms,
                    wrst_ms: s.wrst_ms,
                    stdev_ms: s.stdev_ms,
                });
            }
        }

        MtrCycleOutcome {
            cycle: self.cycles,
            hop_count: samples.len(),
            target_hop: target_sample,
            alerts,
        }
    }

    pub(crate) fn summary(&self) -> MtrSummary {
        MtrSummary {
            cycles: self.cycles,
            probes_per_cycle: self.probes_per_cycle,
            unavailable_reason: self.unavailable_reason.clone(),
            hops: self.hops.values().cloned().collect(),
        }
    }
}

/// Parse stdout from `mtr -r -w -n` into per-hop samples.
pub(crate) fn parse_mtr_report(stdout: &str) -> Vec<MtrHopSample> {
    let mut hops = Vec::new();
    for line in stdout.lines() {
        let Some((hop_part, rest)) = line.split_once(".|--") else {
            continue;
        };
        let Ok(hop) = hop_part.trim().parse::<u8>() else {
            continue;
        };
        let cols: Vec<&str> = rest.split_whitespace().collect();
        if cols.len() < 8 {
            continue;
        }
        let host = cols[0].to_string();
        let loss_str = cols[1].trim_end_matches('%');
        let Ok(loss_pct) = loss_str.parse::<f64>() else {
            continue;
        };
        let Ok(snt) = cols[2].parse::<u64>() else {
            continue;
        };
        let Ok(last_ms) = cols[3].parse::<f64>() else {
            continue;
        };
        let Ok(avg_ms) = cols[4].parse::<f64>() else {
            continue;
        };
        let Ok(best_ms) = cols[5].parse::<f64>() else {
            continue;
        };
        let Ok(wrst_ms) = cols[6].parse::<f64>() else {
            continue;
        };
        let Ok(stdev_ms) = cols[7].parse::<f64>() else {
            continue;
        };

        hops.push(MtrHopSample {
            hop,
            host,
            loss_pct,
            snt,
            last_ms,
            avg_ms,
            best_ms,
            wrst_ms,
            stdev_ms,
        });
    }
    hops
}

/// Format the end-of-session `MTR <ip>:` summary block.
pub(crate) fn format_mtr_summary(target: Ipv4Addr, s: &MtrSummary) -> String {
    if let Some(reason) = &s.unavailable_reason {
        return format!("MTR {}: unavailable ({})", target, reason);
    }
    if s.cycles == 0 {
        return format!(
            "MTR {}: cycles=0 (stopped before first {}-probe cycle completed)",
            target, s.probes_per_cycle
        );
    }

    let mut out = format!(
        "MTR {}: cycles={} probes_per_cycle={}",
        target, s.cycles, s.probes_per_cycle
    );
    for h in &s.hops {
        out.push_str(&format!(
            "\n  hop {:>2}  {:<15}  loss={:>5.1}%  snt={:>4}  avg={:>6.1}ms  best={:>6.1}ms  wrst={:>6.1}ms",
            h.hop,
            h.host,
            h.loss_pct(),
            h.total_snt,
            h.avg_ms(),
            h.best_ms_or_zero(),
            h.wrst_ms
        ));
    }
    out
}

fn spawn_mtr_cycle(target_ip: Ipv4Addr, iface: Option<&str>) -> io::Result<Child> {
    let mut cmd = Command::new("mtr");
    cmd.args([
        "-r",
        "-w",
        "-n",
        "-c",
        &MTR_PROBES_PER_CYCLE.to_string(),
    ]);
    if let Some(ifname) = iface {
        cmd.args(["-I", ifname]);
    }
    cmd.arg(target_ip.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn()
}

fn read_child_pipes(child: &mut Child) -> (String, String) {
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    (stdout, stderr)
}

fn sleep_until_stop(total_ms: u64) {
    let mut remaining = total_ms;
    while remaining > 0 && !STOP.load(Ordering::SeqCst) {
        let step = remaining.min(CHILD_POLL_MS);
        thread::sleep(Duration::from_millis(step));
        remaining -= step;
    }
}

/// Continuously run `mtr -r -w -n` cycles against `target_ip` until `STOP` is signaled.
pub(crate) fn monitor_mtr(
    target_ip: Ipv4Addr,
    spike_ms: f64,
    iface: Option<&str>,
) -> io::Result<MtrSummary> {
    let mut tracker = MtrTracker::new(target_ip, spike_ms, MTR_PROBES_PER_CYCLE);
    let mut active_iface = iface.map(str::to_string);

    while !STOP.load(Ordering::SeqCst) {
        let window_start = ts_now();
        let mut child = match spawn_mtr_cycle(target_ip, active_iface.as_deref()) {
            Ok(c) => c,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let msg = "mtr binary not found in PATH".to_string();
                log_line("MTR", &format!("unavailable: {msg}"));
                tracker.set_unavailable(msg);
                break;
            }
            Err(e) => {
                log_line("MTR", &format!("spawn failed: {e}"));
                sleep_until_stop(ERROR_RETRY_MS);
                continue;
            }
        };

        let status = loop {
            if STOP.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(tracker.summary());
            }
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) => thread::sleep(Duration::from_millis(CHILD_POLL_MS)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
            }
        };

        let window_end = ts_now();
        let (stdout, stderr) = read_child_pipes(&mut child);

        if !status.success() {
            if let Some(ifname) = active_iface.take() {
                log_line(
                    "MTR",
                    &format!(
                        "mtr failed with -I {ifname} ({}); retrying without interface bind",
                        stderr.trim()
                    ),
                );
                continue;
            }
            let detail = stderr.trim();
            log_line(
                "MTR",
                &format!(
                    "cycle failed (status={}): {}",
                    status,
                    if detail.is_empty() { "no stderr" } else { detail }
                ),
            );
            sleep_until_stop(ERROR_RETRY_MS);
            continue;
        }

        let samples = parse_mtr_report(&stdout);
        let outcome = tracker.record_cycle(&samples);
        if let Some(t) = &outcome.target_hop {
            log_line(
                "MTR",
                &format!(
                    "cycle={} window={}..{} hops={} target={} loss={:.1}% avg={:.1}ms wrst={:.1}ms",
                    outcome.cycle,
                    window_start,
                    window_end,
                    outcome.hop_count,
                    t.host,
                    t.loss_pct,
                    t.avg_ms,
                    t.wrst_ms
                ),
            );
        } else {
            log_line(
                "MTR",
                &format!(
                    "cycle={} window={}..{} hops={}",
                    outcome.cycle, window_start, window_end, outcome.hop_count
                ),
            );
        }

        for alert in &outcome.alerts {
            log_line(
                "MTR-HOP",
                &format!(
                    "cycle={} hop={} ip={} loss={:.1}% last={:.1}ms avg={:.1}ms best={:.1}ms wrst={:.1}ms stdev={:.1}ms",
                    outcome.cycle,
                    alert.hop,
                    alert.host,
                    alert.loss_pct,
                    alert.last_ms,
                    alert.avg_ms,
                    alert.best_ms,
                    alert.wrst_ms,
                    alert.stdev_ms
                ),
            );
        }
    }

    Ok(tracker.summary())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mtr_wide_numeric_report_and_ignores_non_hop_lines() {
        let sample = "\
Start: 2026-11-25T21:59:00-0800
HOST: desktop                     Loss%   Snt   Last   Avg  Best  Wrst StDev
  1.|-- 192.168.1.1                0.0%     5    0.4   0.4   0.3   0.5   0.1
  2.|-- ???                       100.0     5    0.0   0.0   0.0   0.0   0.0
  3.|-- 10.20.0.1                 20.0%     5   12.3  15.4  11.0  24.2   5.1
        10.20.0.2
        [MPLS: Lbl 12345 TC 0 S 1 TTL 1]
  4.|-- 92.223.1.10                0.0%     5   31.2  32.0  30.5  34.1   1.4
";
        let hops = parse_mtr_report(sample);
        assert_eq!(hops.len(), 4);

        assert_eq!(hops[0].hop, 1);
        assert_eq!(hops[0].host, "192.168.1.1");
        assert!((hops[0].loss_pct - 0.0).abs() < 1e-6);
        assert_eq!(hops[0].snt, 5);

        assert_eq!(hops[1].hop, 2);
        assert_eq!(hops[1].host, "???");
        assert!((hops[1].loss_pct - 100.0).abs() < 1e-6);

        assert_eq!(hops[2].hop, 3);
        assert_eq!(hops[2].host, "10.20.0.1");
        assert!((hops[2].loss_pct - 20.0).abs() < 1e-6);
        assert!((hops[2].wrst_ms - 24.2).abs() < 1e-6);

        assert_eq!(hops[3].hop, 4);
        assert_eq!(hops[3].host, "92.223.1.10");
        assert!((hops[3].avg_ms - 32.0).abs() < 1e-6);
    }

    #[test]
    fn suppresses_persistent_silent_hops_but_alerts_on_loss_and_spikes() {
        let target = Ipv4Addr::new(92, 223, 1, 10);
        let mut tracker = MtrTracker::new(target, 80.0, 5);

        let cycle1 = vec![
            MtrHopSample {
                hop: 1,
                host: "192.168.1.1".to_string(),
                loss_pct: 0.0,
                snt: 5,
                last_ms: 0.5,
                avg_ms: 0.5,
                best_ms: 0.4,
                wrst_ms: 0.7,
                stdev_ms: 0.1,
            },
            MtrHopSample {
                hop: 2,
                host: "???".to_string(),
                loss_pct: 100.0,
                snt: 5,
                last_ms: 0.0,
                avg_ms: 0.0,
                best_ms: 0.0,
                wrst_ms: 0.0,
                stdev_ms: 0.0,
            },
            MtrHopSample {
                hop: 3,
                host: "92.223.1.10".to_string(),
                loss_pct: 0.0,
                snt: 5,
                last_ms: 30.0,
                avg_ms: 30.0,
                best_ms: 29.0,
                wrst_ms: 31.0,
                stdev_ms: 0.8,
            },
        ];

        let out1 = tracker.record_cycle(&cycle1);
        assert_eq!(out1.cycle, 1);
        assert!(out1.alerts.is_empty());

        let cycle2 = vec![
            MtrHopSample {
                hop: 1,
                host: "192.168.1.1".to_string(),
                loss_pct: 100.0, // previously responded, now 100% lost
                snt: 5,
                last_ms: 0.0,
                avg_ms: 0.0,
                best_ms: 0.0,
                wrst_ms: 0.0,
                stdev_ms: 0.0,
            },
            MtrHopSample {
                hop: 2,
                host: "???".to_string(),
                loss_pct: 100.0, // still silent, must not alert
                snt: 5,
                last_ms: 0.0,
                avg_ms: 0.0,
                best_ms: 0.0,
                wrst_ms: 0.0,
                stdev_ms: 0.0,
            },
            MtrHopSample {
                hop: 3,
                host: "92.223.1.10".to_string(),
                loss_pct: 20.0,
                snt: 5,
                last_ms: 145.0,
                avg_ms: 60.0,
                best_ms: 29.5,
                wrst_ms: 145.0,
                stdev_ms: 49.0,
            },
        ];

        let out2 = tracker.record_cycle(&cycle2);
        assert_eq!(out2.cycle, 2);
        assert_eq!(out2.alerts.len(), 2);
        assert_eq!(out2.alerts[0].hop, 1);
        assert_eq!(out2.alerts[1].hop, 3);

        let summary = tracker.summary();
        assert_eq!(summary.cycles, 2);
        assert_eq!(summary.hops[0].total_snt, 10);
        assert!((summary.hops[0].loss_pct() - 50.0).abs() < 1e-6);
        assert_eq!(summary.hops[2].total_snt, 10);
        assert!((summary.hops[2].loss_pct() - 10.0).abs() < 1e-6);
        assert!((summary.hops[2].best_ms_or_zero() - 29.0).abs() < 1e-6);
        assert!((summary.hops[2].wrst_ms - 145.0).abs() < 1e-6);
    }

    #[test]
    fn formats_zero_cycle_and_populated_mtr_summary() {
        let target = Ipv4Addr::new(92, 223, 1, 10);
        let empty = MtrTracker::new(target, 80.0, 5).summary();
        assert_eq!(
            format_mtr_summary(target, &empty),
            "MTR 92.223.1.10: cycles=0 (stopped before first 5-probe cycle completed)"
        );

        let mut tracker = MtrTracker::new(target, 80.0, 5);
        tracker.record_cycle(&[MtrHopSample {
            hop: 1,
            host: "92.223.1.10".to_string(),
            loss_pct: 0.0,
            snt: 5,
            last_ms: 30.0,
            avg_ms: 30.0,
            best_ms: 28.0,
            wrst_ms: 32.0,
            stdev_ms: 1.0,
        }]);
        let rendered = format_mtr_summary(target, &tracker.summary());
        assert!(rendered.contains("MTR 92.223.1.10: cycles=1 probes_per_cycle=5"));
        assert!(rendered.contains("hop  1  92.223.1.10"));
    }
}
