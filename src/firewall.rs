use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Max strikes before an IP gets blacklisted.
const MAX_STRIKES: u32 = 5;

/// Max packets per second from a single IP before it counts as a strike.
/// Raised to 1000 to accommodate screen sharing (~200-400 chunks/s + ~50 audio pkt/s).
const RATE_LIMIT: u32 = 1000;

/// Tracks per-IP state for rate limiting and blacklisting.
struct IpState {
    strikes: u32,
    packet_count: u32,
    window_start: Instant,
}

/// Firewall: rate limiter + auto-blacklist.
///
/// Call `check()` on every incoming packet. It returns:
/// - `Action::Allow` → process the packet
/// - `Action::Deny` → silently drop it
///
/// Call `record_failure()` when a packet fails authentication (bad decrypt).
pub struct Firewall {
    ips: HashMap<IpAddr, IpState>,
    blacklist: Vec<IpAddr>,
    auto_ban_sink: Option<Arc<Mutex<Vec<String>>>>,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Allow,
    Deny,
}

impl Firewall {
    /// Create a firewall pre-populated with banned IPs and an auto-ban sink.
    /// The sink receives IP strings whenever the firewall auto-blacklists someone.
    pub fn new_with_bans(banned: &[String], sink: Arc<Mutex<Vec<String>>>) -> Self {
        let mut blacklist = Vec::new();
        for ip_str in banned {
            if let Ok(ip) = ip_str.parse::<IpAddr>() {
                blacklist.push(ip);
            }
        }
        Self {
            ips: HashMap::new(),
            blacklist,
            auto_ban_sink: Some(sink),
        }
    }

    /// Check if a packet from this IP should be allowed.
    /// Also updates rate counters.
    pub fn check(&mut self, ip: IpAddr) -> Action {
        // Fast path: blacklisted IPs are always denied
        if self.blacklist.contains(&ip) {
            return Action::Deny;
        }

        let now = Instant::now();

        let state = self.ips.entry(ip).or_insert_with(|| IpState {
            strikes: 0,
            packet_count: 0,
            window_start: now,
        });

        // Reset rate counter every second
        if now.duration_since(state.window_start).as_secs() >= 1 {
            state.packet_count = 0;
            state.window_start = now;
        }

        state.packet_count += 1;

        // Rate limit exceeded → strike
        if state.packet_count > RATE_LIMIT {
            state.strikes += 1;
            if state.strikes >= MAX_STRIKES {
                self.blacklist.push(ip);
                if let Some(ref sink) = self.auto_ban_sink {
                    if let Ok(mut v) = sink.lock() {
                        v.push(ip.to_string());
                    }
                }
                eprintln!("[firewall] BLACKLISTED {ip} (rate limit exceeded)");
                return Action::Deny;
            }
        }

        Action::Allow
    }

    /// Record an authentication failure (bad decrypt, invalid packet).
    /// Called when a packet passes rate check but fails crypto validation.
    pub fn record_failure(&mut self, ip: IpAddr) {
        if self.blacklist.contains(&ip) {
            return;
        }

        let now = Instant::now();
        let state = self.ips.entry(ip).or_insert_with(|| IpState {
            strikes: 0,
            packet_count: 0,
            window_start: now,
        });

        state.strikes += 1;

        if state.strikes >= MAX_STRIKES {
            self.blacklist.push(ip);
            if let Some(ref sink) = self.auto_ban_sink {
                if let Ok(mut v) = sink.lock() {
                    v.push(ip.to_string());
                }
            }
            eprintln!("[firewall] BLACKLISTED {ip} (auth failures: {})", state.strikes);
        } else {
            eprintln!(
                "[firewall] Strike {}/{MAX_STRIKES} for {ip} (auth failure)",
                state.strikes
            );
        }
    }

}
