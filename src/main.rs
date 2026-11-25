mod discovery;
mod ping;
mod queue_counters;
mod udp_monitor;

use std::env;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;

pub(crate) static STOP: AtomicBool = AtomicBool::new(false);
static LOG_LOCK: Mutex<()> = Mutex::new(());

extern "C" fn on_sigint(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// RAII wrapper ensuring raw socket file descriptors are closed on drop.
pub(crate) struct Fd(pub libc::c_int);
impl Drop for Fd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

/// Format current UTC wall-clock time-of-day as `HH:MM:SS.ffffff`.
pub(crate) fn ts_now() -> String {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    unsafe {
        libc::gettimeofday(&mut tv, std::ptr::null_mut());
    }
    let tod = tv.tv_sec.rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}.{:06}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        tv.tv_usec
    )
}

/// Print a single timestamped diagnostic event line atomically across threads.
pub(crate) fn log_line(tag: &str, body: &str) {
    let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    println!("[{tag}] {} {body}", ts_now());
}

/// Return `Some(v4)` only when `ip` is a routable public IPv4 address.
pub(crate) fn is_pub_v4(ip: IpAddr) -> Option<Ipv4Addr> {
    match ip {
        IpAddr::V4(v)
            if !v.is_loopback()
                && !v.is_private()
                && !v.is_multicast()
                && !v.is_broadcast()
                && !v.is_unspecified()
                && !v.is_link_local() =>
        {
            Some(v)
        }
        _ => None,
    }
}

fn parse_port_arg(arg: Option<&str>) -> Result<Option<u16>, std::num::ParseIntError> {
    match arg {
        None | Some("") => Ok(Some(32862)),
        Some("any" | "*") => Ok(None),
        Some(s) => s.parse().map(Some),
    }
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
        println!(
            "Usage: game_ping_watch [secs=60] [interval_ms=200] [spike_ms=80] [port=32862|any] [iface]"
        );
        return Ok(());
    }

    let mut args = raw_args.into_iter();
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
    let interval: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(200);
    let spike: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(80.0);
    let port = parse_port_arg(args.next().as_deref())?;
    let iface: Option<String> = args.next().filter(|s| !s.is_empty());

    eprintln!(
        "Discovering server for {}s (port={:?}, iface={})...",
        secs,
        port,
        iface.as_deref().unwrap_or("all")
    );

    if let Some(res) = discovery::discover_ip(secs, port, iface.as_deref())? {
        let timeout_ms = 1000;
        let ports_display = discovery::format_ports_list(&res.udp_ports, "waiting for UDP");
        println!(
            "\nPinging {} every {}ms (spike >= {:.1}ms, timeout={}ms).",
            res.target_ip, interval, spike, timeout_ms
        );
        println!(
            "Monitoring UDP for {} on ports [{}]. Ctrl+C to stop.",
            res.target_ip, ports_display
        );

        let target_ip = res.target_ip;
        let initial_ports = res.udp_ports;
        let udp_iface = iface.clone();
        let udp_handle = thread::spawn(move || {
            udp_monitor::monitor_udp(target_ip, initial_ports, port, udp_iface.as_deref())
        });

        let ping_res = ping::ping_loop(target_ip, interval, spike, timeout_ms);
        if ping_res.is_err() {
            STOP.store(true, Ordering::SeqCst);
        }

        let udp_res = udp_handle
            .join()
            .map_err(|_| std::io::Error::other("UDP monitor thread panicked"))?;

        ping_res?;
        let summary = udp_res?;
        println!("{}", udp_monitor::format_udp_summary(target_ip, &summary));
    } else {
        eprintln!("No public IPv4 candidate found.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_port_argument_variants() {
        assert_eq!(parse_port_arg(None).unwrap(), Some(32862));
        assert_eq!(parse_port_arg(Some("")).unwrap(), Some(32862));
        assert_eq!(parse_port_arg(Some("any")).unwrap(), None);
        assert_eq!(parse_port_arg(Some("*")).unwrap(), None);
        assert_eq!(parse_port_arg(Some("32862")).unwrap(), Some(32862));
        assert!(parse_port_arg(Some("bad")).is_err());
    }
}
