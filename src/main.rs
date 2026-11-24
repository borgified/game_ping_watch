use etherparse::{NetSlice, SlicedPacket, TransportSlice};
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::CString;
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const ETH_P_IP: u16 = 0x0800;
const ETH_P_8021Q: u16 = 0x8100;
const ICMP_ECHO_REQUEST: u8 = 8;
const ICMP_ECHO_REPLY: u8 = 0;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

struct Fd(libc::c_int);
impl Drop for Fd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0); }
    }
}

fn ts_now() -> String {
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()); }
    let tod = tv.tv_sec.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}.{:06}", tod / 3600, (tod % 3600) / 60, tod % 60, tv.tv_usec)
}

fn is_pub_v4(ip: IpAddr) -> Option<Ipv4Addr> {
    match ip {
        IpAddr::V4(v) if !v.is_loopback() && !v.is_private() && !v.is_multicast()
            && !v.is_broadcast() && !v.is_unspecified() && !v.is_link_local() => Some(v),
        _ => None,
    }
}

fn checksum(buf: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < buf.len() {
        sum += u16::from_be_bytes([buf[i], buf[i + 1]]) as u32;
        i += 2;
    }
    if i < buf.len() { sum += (buf[i] as u32) << 8; }
    while (sum >> 16) != 0 { sum = (sum & 0xFFFF) + (sum >> 16); }
    !(sum as u16)
}

fn discover_ip(secs: u64, port: Option<u16>, iface: Option<&str>) -> io::Result<Option<Ipv4Addr>> {
    let proto_be = (libc::ETH_P_ALL as u16).to_be();
    let fd = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_RAW, proto_be as libc::c_int) };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    let _fd = Fd(fd);

    if let Some(name) = iface {
        let c_name = CString::new(name)
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "invalid interface name"))?;
        let idx = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
        if idx == 0 {
            let e = io::Error::last_os_error();
            return Err(if e.raw_os_error() == Some(0) {
                io::Error::new(ErrorKind::NotFound, format!("unknown interface: {name}"))
            } else {
                e
            });
        }
        let mut sll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        sll.sll_family = libc::AF_PACKET as u16;
        sll.sll_protocol = proto_be;
        sll.sll_ifindex = idx as libc::c_int;
        let rc = unsafe {
            libc::bind(
                fd,
                &sll as *const libc::sockaddr_ll as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            )
        };
        if rc < 0 { return Err(io::Error::last_os_error()); }
    }

    let rcv_to = libc::timeval { tv_sec: 0, tv_usec: 200_000 };
    unsafe {
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO,
            &rcv_to as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t);
    }

    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut frame_buf = vec![0u8; 65536];
    let mut counts: HashMap<Ipv4Addr, (u64, u64, u16)> = HashMap::new();

    while !STOP.load(Ordering::SeqCst) && Instant::now() < deadline {
        let n = unsafe {
            libc::recvfrom(fd, frame_buf.as_mut_ptr() as *mut libc::c_void, frame_buf.len(), 0,
                std::ptr::null_mut(), std::ptr::null_mut())
        };
        if n <= 0 {
            let e = io::Error::last_os_error();
            match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted => continue,
                _ => return Err(e),
            }
        }
        let f = &frame_buf[..n as usize];
        if f.len() < 14 { continue; }
        let ip_slice = match u16::from_be_bytes([f[12], f[13]]) {
            ETH_P_IP => &f[14..],
            ETH_P_8021Q if f.len() >= 18 && u16::from_be_bytes([f[16], f[17]]) == ETH_P_IP => &f[18..],
            _ => continue,
        };
        let Ok(sliced) = SlicedPacket::from_ip(ip_slice) else { continue; };

        let (src, dst) = match &sliced.net {
            Some(NetSlice::Ipv4(v4)) => (IpAddr::V4(v4.header().source_addr()), IpAddr::V4(v4.header().destination_addr())),
            _ => continue,
        };
        let (sp, dp) = match sliced.transport {
            Some(TransportSlice::Udp(u)) => (u.source_port(), u.destination_port()),
            Some(TransportSlice::Tcp(t)) => (t.source_port(), t.destination_port()),
            _ => continue,
        };
        if matches!(sp, 53 | 1900 | 5353) || matches!(dp, 53 | 1900 | 5353) { continue; }
        if let Some(p) = port { if sp != p && dp != p { continue; } }

        let (rip, rport) = if let Some(v) = is_pub_v4(src) { (v, sp) }
        else if let Some(v) = is_pub_v4(dst) { (v, dp) }
        else { continue; };

        let entry = counts.entry(rip).or_insert((0, 0, rport));
        entry.0 += 1;
        entry.1 += f.len() as u64;
        entry.2 = rport;
    }

    let mut top: Vec<_> = counts.into_iter().collect();
    top.sort_by(|a, b| b.1.1.cmp(&a.1.1));
    eprintln!("\nTop IPv4 candidates:");
    for (ip, (pkts, bytes, p)) in top.iter().take(5) {
        eprintln!("  {:<15}:{:<5} pkts={:<6} bytes={}", ip, p, pkts, bytes);
    }
    Ok(top.first().map(|(ip, _)| *ip))
}

fn ping_loop(target: Ipv4Addr, interval_ms: u64, spike_ms: f64, timeout_ms: u64) -> io::Result<()> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_ICMP as libc::c_int) };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    let _fd = Fd(fd);

    let rcv_to = libc::timeval { tv_sec: 0, tv_usec: 20_000 };
    unsafe {
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO,
            &rcv_to as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t);
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

    println!("\nPinging {} every {}ms (spike >= {:.1}ms, timeout={}ms). Ctrl+C to stop.",
        target, interval_ms, spike_ms, timeout_ms);

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
                libc::sendto(fd, pkt.as_ptr() as *const libc::c_void, pkt.len(), 0,
                    &addr as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t)
            };
            if r >= 0 {
                sent += 1;
                sent_map.insert(seq, now);
                pending.push(seq);
            }
            next_tx = now + Duration::from_millis(interval_ms);
        }

        let n = unsafe {
            libc::recvfrom(fd, rx_buf.as_mut_ptr() as *mut libc::c_void, rx_buf.len(), 0,
                std::ptr::null_mut(), std::ptr::null_mut())
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
                                println!("[SPIKE] {} seq={} rtt={:.2}ms delta={:.2}ms", ts_now(), rseq, rtt, delta);
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
                        println!("[TIMEOUT] {} seq={} > {}ms", ts_now(), s, timeout_ms);
                    }
                    false
                }
                Some(false) => true,
                None => false,
            }
        });
    }

    let loss = if sent > 0 { (timeouts as f64 / sent as f64) * 100.0 } else { 0.0 };
    let avg = if recv > 0 { sum_rtt / recv as f64 } else { 0.0 };
    let min_v = if min_rtt.is_finite() { min_rtt } else { 0.0 };
    println!("\nSummary {}: sent={} recv={} timeouts={} loss={:.1}% spikes={}", target, sent, recv, timeouts, loss, spikes);
    println!("RTT ms: min={:.2} avg={:.2} max={:.2}", min_v, avg, max_rtt);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigint as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }

    let raw_args: Vec<String> = env::args().skip(1).collect();
    if raw_args.iter().any(|a| a == "-h" || a == "--help") {
        println!("Usage: game_ping_watch [secs=60] [interval_ms=200] [spike_ms=80] [port=32862|any] [iface]");
        return Ok(());
    }

    let mut args = raw_args.into_iter();
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
    let interval: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(200);
    let spike: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(80.0);
    let port: Option<u16> = match args.next().as_deref() {
        None | Some("") => Some(32862),
        Some("any" | "*") => None,
        Some(s) => Some(s.parse()?),
    };
    let iface: Option<String> = args.next().filter(|s| !s.is_empty());

    eprintln!("Discovering server for {}s (port={:?}, iface={})...",
        secs, port, iface.as_deref().unwrap_or("all"));
    if let Some(ip) = discover_ip(secs, port, iface.as_deref())? {
        ping_loop(ip, interval, spike, 1000)?;
    } else {
        eprintln!("No public IPv4 candidate found.");
    }
    Ok(())
}
