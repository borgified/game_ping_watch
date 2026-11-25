use std::io;
use std::net::Ipv4Addr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::{log_line, Fd, STOP};

const ICMP_ECHO_REQUEST: u8 = 8;
const ICMP_ECHO_REPLY: u8 = 0;

/// Compute the RFC 1071 Internet checksum for an ICMP packet buffer.
pub(crate) fn checksum(buf: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < buf.len() {
        sum += u16::from_be_bytes([buf[i], buf[i + 1]]) as u32;
        i += 2;
    }
    if i < buf.len() {
        sum += (buf[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Continuously send ICMP Echo Requests to `target` and log spikes, timeouts, and summary stats.
pub(crate) fn ping_loop(
    target: Ipv4Addr,
    interval_ms: u64,
    spike_ms: f64,
    timeout_ms: u64,
) -> io::Result<()> {
    use std::collections::{HashMap, HashSet};
    use std::net::SocketAddrV4;

    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_ICMP as libc::c_int) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let _fd = Fd(fd);

    let rcv_to = libc::timeval {
        tv_sec: 0,
        tv_usec: 20_000,
    };
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &rcv_to as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
    }

    let ident = (std::process::id() & 0xFFFF) as u16;
    let mut seq: u16 = 0;
    let mut sent_map: HashMap<u16, Instant> = HashMap::new();
    let mut pending: Vec<u16> = Vec::new();
    let mut next_tx = Instant::now();
    let mut prev_rtt: Option<f64> = None;
    let mut timed_out_seqs: HashSet<u16> = HashSet::new();

    let (mut sent, mut recv, mut timeouts, mut spikes) = (0u64, 0u64, 0u64, 0u64);
    let (mut min_rtt, mut max_rtt, mut sum_rtt) = (f64::INFINITY, 0.0f64, 0.0f64);

    let _ = SocketAddrV4::new(target, 0);
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    addr.sin_family = libc::AF_INET as u16;
    addr.sin_addr.s_addr = u32::from(target).to_be();

    let mut rx_buf = vec![0u8; 1500];
    while !STOP.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= next_tx {
            seq = seq.wrapping_add(1);
            let mut pkt = vec![ICMP_ECHO_REQUEST, 0, 0, 0];
            pkt.extend_from_slice(&ident.to_be_bytes());
            pkt.extend_from_slice(&seq.to_be_bytes());
            pkt.extend_from_slice(b"probe");
            let csum = checksum(&pkt);
            pkt[2..4].copy_from_slice(&csum.to_be_bytes());

            let r = unsafe {
                libc::sendto(
                    fd,
                    pkt.as_ptr() as *const libc::c_void,
                    pkt.len(),
                    0,
                    &addr as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            };
            if r >= 0 {
                sent += 1;
                sent_map.insert(seq, now);
                pending.push(seq);
            }
            next_tx = now + Duration::from_millis(interval_ms);
        }

        let n = unsafe {
            libc::recvfrom(
                fd,
                rx_buf.as_mut_ptr() as *mut libc::c_void,
                rx_buf.len(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if n > 0 {
            let rx_t = Instant::now();
            let p = &rx_buf[..n as usize];
            if p.len() >= 28 {
                let ihl = ((p[0] & 0x0F) as usize) * 4;
                if p.len() >= ihl + 8 {
                    let src = Ipv4Addr::new(p[12], p[13], p[14], p[15]);
                    let icmp = &p[ihl..];
                    let id = u16::from_be_bytes([icmp[4], icmp[5]]);
                    let rseq = u16::from_be_bytes([icmp[6], icmp[7]]);
                    if src == target && icmp[0] == ICMP_ECHO_REPLY && id == ident {
                        if let Some(tx_t) = sent_map.remove(&rseq) {
                            let rtt = rx_t.duration_since(tx_t).as_secs_f64() * 1000.0;
                            recv += 1;
                            sum_rtt += rtt;
                            min_rtt = min_rtt.min(rtt);
                            max_rtt = max_rtt.max(rtt);
                            let delta = prev_rtt.map(|x| (rtt - x).abs()).unwrap_or(0.0);
                            prev_rtt = Some(rtt);
                            if rtt >= spike_ms || delta >= spike_ms {
                                spikes += 1;
                                log_line(
                                    "SPIKE",
                                    &format!("seq={rseq} rtt={rtt:.2}ms delta={delta:.2}ms"),
                                );
                            }
                        }
                    }
                }
            }
        }

        let check_t = Instant::now();
        let to_dur = Duration::from_millis(timeout_ms);
        pending.retain(|&s| {
            let expired = sent_map.get(&s).map(|&t| check_t.duration_since(t) >= to_dur);
            match expired {
                Some(true) => {
                    sent_map.remove(&s);
                    if timed_out_seqs.insert(s) {
                        timeouts += 1;
                        log_line("TIMEOUT", &format!("seq={s} > {timeout_ms}ms"));
                    }
                    false
                }
                Some(false) => true,
                None => false,
            }
        });
    }

    let loss = if sent > 0 {
        (timeouts as f64 / sent as f64) * 100.0
    } else {
        0.0
    };
    let avg = if recv > 0 { sum_rtt / recv as f64 } else { 0.0 };
    let min_v = if min_rtt.is_finite() { min_rtt } else { 0.0 };
    println!(
        "\nSummary {}: sent={} recv={} timeouts={} loss={:.1}% spikes={}",
        target, sent, recv, timeouts, loss, spikes
    );
    println!("RTT ms: min={:.2} avg={:.2} max={:.2}", min_v, avg, max_rtt);
    Ok(())
}
