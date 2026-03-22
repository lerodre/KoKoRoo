use cpal::traits::{DeviceTrait, HostTrait};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::voice;

/// Collects IPv6 addresses, tagged by scope (global vs link-local).
fn get_ipv6_addresses() -> Vec<(String, String)> {
    // Returns Vec<(ip, label)>
    let mut addrs = vec![("::1".to_string(), "::1 (loopback)".to_string())];

    if let Ok(output) = std::process::Command::new("ip")
        .args(["-6", "addr", "show"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("inet6") {
                if let Some(addr_cidr) = trimmed.split_whitespace().nth(1) {
                    let addr = addr_cidr.split('/').next().unwrap_or(addr_cidr);
                    if addr != "::1" {
                        let scope = if trimmed.contains("scope global") {
                            "global/internet"
                        } else if trimmed.contains("scope link") {
                            "link-local/LAN"
                        } else {
                            "other"
                        };
                        addrs.push((addr.to_string(), format!("{addr} ({scope})")));
                    }
                }
            }
        }
    }

    addrs
}

/// Interactive arrow-key menu. Returns selected index.
fn select_menu(title: &str, items: &[String]) -> usize {
    let mut selected: usize = 0;

    terminal::enable_raw_mode().expect("Failed to enable raw mode");

    loop {
        print!("\x1b[2J\x1b[H");
        println!("\x1b[1;36m=== KoKoRoo — Secure P2P Voice ===\x1b[0m\r");
        println!("\r");
        println!("\x1b[1m{title}\x1b[0m\r");
        println!("\x1b[90m(Arrow keys to move, Enter to select, Q to quit)\x1b[0m\r");
        println!("\r");

        for (i, item) in items.iter().enumerate() {
            if i == selected {
                println!("  \x1b[1;32m> {item}\x1b[0m\r");
            } else {
                println!("    {item}\r");
            }
        }

        io::stdout().flush().unwrap();

        if let Ok(Event::Key(key)) = event::read() {
            match key.code {
                KeyCode::Up => { if selected > 0 { selected -= 1; } }
                KeyCode::Down => { if selected < items.len() - 1 { selected += 1; } }
                KeyCode::Enter => {
                    terminal::disable_raw_mode().unwrap();
                    return selected;
                }
                KeyCode::Char('q') => {
                    terminal::disable_raw_mode().unwrap();
                    std::process::exit(0);
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    terminal::disable_raw_mode().unwrap();
                    std::process::exit(0);
                }
                _ => {}
            }
        }
    }
}

/// Text input prompt.
fn input_prompt(prompt: &str, default: &str) -> String {
    print!("{prompt} [{default}]: ");
    io::stdout().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let trimmed = input.trim();
    if trimmed.is_empty() { default.to_string() } else { trimmed.to_string() }
}

/// Main interactive UI.
pub fn run() {
    let host = cpal::default_host();

    // ── Step 1: Network Mode ──
    let mode_items = vec![
        "LAN (link-local IPv6 — same network only)".to_string(),
        "Internet (global IPv6 — across the internet)".to_string(),
    ];
    let mode_idx = select_menu("Select network mode:", &mode_items);
    let is_internet = mode_idx == 1;

    // ── Step 2: Select IPv6 Address ──
    let all_addrs = get_ipv6_addresses();

    // Filter by mode
    let filtered: Vec<&(String, String)> = all_addrs.iter().filter(|(_, label)| {
        if is_internet {
            label.contains("global") || label.contains("loopback")
        } else {
            label.contains("link-local") || label.contains("loopback")
        }
    }).collect();

    let addr_labels: Vec<String> = filtered.iter().map(|(_, l)| l.clone()).collect();

    if addr_labels.is_empty() {
        eprintln!("No {} IPv6 addresses found on this machine.",
            if is_internet { "global" } else { "link-local" });
        eprintln!("Check your network configuration.");
        return;
    }

    let addr_idx = select_menu("Select your IPv6 address:", &addr_labels);
    let _selected_ip = &filtered[addr_idx].0;

    // ── Step 3: Select Output Device ──
    let output_devices: Vec<_> = host.output_devices()
        .expect("Failed to list output devices").collect();
    let output_names: Vec<String> = output_devices.iter().enumerate()
        .map(|(i, d)| {
            let name = d.name().unwrap_or_else(|_| "unknown".into());
            if i == 0 { format!("{name} (default)") } else { name }
        }).collect();
    let out_idx = select_menu("Select audio output (speakers/headphones):", &output_names);
    let output_device = &output_devices[out_idx];

    // ── Step 4: Select Input Device ──
    let input_devices: Vec<_> = host.input_devices()
        .expect("Failed to list input devices").collect();
    let input_names: Vec<String> = input_devices.iter().enumerate()
        .map(|(i, d)| {
            let name = d.name().unwrap_or_else(|_| "unknown".into());
            if i == 0 { format!("{name} (default)") } else { name }
        }).collect();
    let in_idx = select_menu("Select audio input (microphone):", &input_names);
    let input_device = &input_devices[in_idx];

    // ── Step 5: Connection Details ──
    print!("\x1b[2J\x1b[H");
    println!("\x1b[1;36m=== KoKoRoo — Connection Setup ===\x1b[0m");
    println!();
    if is_internet {
        println!("\x1b[1;33m! Internet mode: make sure port is open in your firewall\x1b[0m");
        println!();
    }

    let local_port = input_prompt("Your port", "9000");
    let peer_ip = input_prompt("Peer IPv6 address", "::1");
    let peer_port = input_prompt("Peer port", "9000");

    // ── Step 6: Key Exchange + Call ──
    let peer_addr = format!("[{peer_ip}]:{peer_port}");

    println!();
    println!("Connecting to peer and performing key exchange...");
    println!("(waiting up to 30 seconds for peer)");

    let identity = crate::identity::Identity::load_or_create();
    let settings = crate::identity::Settings::load();
    let running = Arc::new(AtomicBool::new(true));
    let mic_active = Arc::new(AtomicBool::new(true));

    if let Ok(port_num) = local_port.parse::<u16>() {
        match crate::platform::ensure_udp_port_open(port_num) {
            Ok(true) => log_fmt!("[firewall] Added rule for UDP port {}", port_num),
            Ok(false) => log_fmt!("[firewall] Rule already exists for UDP port {}", port_num),
            Err(e) => log_fmt!("[firewall] WARNING: {}", e),
        }
    }

    let engine = match voice::start_engine(
        input_device, output_device,
        &peer_addr, &local_port,
        running.clone(), mic_active.clone(),
        &identity,
        &settings.nickname,
    ) {
        Ok(mut e) => {
            // TUI mode: auto-confirm pending contact (warning is printed to terminal)
            if let Some(ref warning) = e.key_change_warning {
                eprintln!("\x1b[1;31m{warning}\x1b[0m");
            }
            e.confirm_contact();
            e
        }
        Err(e) => {
            eprintln!("\x1b[1;31mFailed: {e}\x1b[0m");
            eprintln!("Make sure your peer is also running KoKoRoo.");
            return;
        }
    };

    // ── Step 7: Live Call Screen ──
    terminal::enable_raw_mode().unwrap();
    draw_call_screen(&peer_addr, &local_port, is_internet, &engine.verification_code, true);

    while running.load(Ordering::Relaxed) {
        if event::poll(std::time::Duration::from_millis(200)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                match key.code {
                    KeyCode::Char(' ') => {
                        let was = mic_active.load(Ordering::Relaxed);
                        mic_active.store(!was, Ordering::Relaxed);
                        draw_call_screen(&peer_addr, &local_port, is_internet, &engine.verification_code, !was);
                    }
                    KeyCode::Char('q') | KeyCode::Esc => {
                        running.store(false, Ordering::Relaxed);
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        running.store(false, Ordering::Relaxed);
                    }
                    _ => {}
                }
            }
        }
    }

    terminal::disable_raw_mode().unwrap();
    print!("\x1b[2J\x1b[H");
    println!("Call ended. Goodbye!");
}

/// Draw the call status screen with security info.
fn draw_call_screen(peer: &str, port: &str, internet: bool, verify_code: &str, mic_on: bool) {
    print!("\x1b[2J\x1b[H");

    let mic_status = if mic_on {
        "\x1b[1;32m ON \x1b[0m"
    } else {
        "\x1b[1;31m OFF (MUTED) \x1b[0m"
    };
    let mic_icon = if mic_on { "[|||]" } else { "[ X ]" };
    let mode = if internet { "INTERNET" } else { "LAN" };
    let lock = "\x1b[1;32mENCRYPTED\x1b[0m";

    println!("\x1b[1;36m╔════════════════════════════════════════════╗\x1b[0m\r");
    println!("\x1b[1;36m║        KoKoRoo — Secure Voice Call         ║\x1b[0m\r");
    println!("\x1b[1;36m╠════════════════════════════════════════════╣\x1b[0m\r");
    println!("\x1b[1;36m║\x1b[0m  Peer:   {:<33}\x1b[1;36m║\x1b[0m\r", peer);
    println!("\x1b[1;36m║\x1b[0m  Port:   {:<33}\x1b[1;36m║\x1b[0m\r", port);
    println!("\x1b[1;36m║\x1b[0m  Mode:   {:<33}\x1b[1;36m║\x1b[0m\r", mode);
    println!("\x1b[1;36m║\x1b[0m  Codec:  {:<33}\x1b[1;36m║\x1b[0m\r", "Opus 64kbps");
    println!("\x1b[1;36m║\x1b[0m  Status: {lock}                           \x1b[1;36m║\x1b[0m\r");
    println!("\x1b[1;36m╠════════════════════════════════════════════╣\x1b[0m\r");
    println!("\x1b[1;36m║\x1b[0m                                            \x1b[1;36m║\x1b[0m\r");
    println!("\x1b[1;36m║\x1b[0m  Verify: \x1b[1;33m{verify_code}\x1b[0m                            \x1b[1;36m║\x1b[0m\r");
    println!("\x1b[1;36m║\x1b[0m  ^ Ask your peer for their code.           \x1b[1;36m║\x1b[0m\r");
    println!("\x1b[1;36m║\x1b[0m    If it matches, no one is listening.     \x1b[1;36m║\x1b[0m\r");
    println!("\x1b[1;36m║\x1b[0m                                            \x1b[1;36m║\x1b[0m\r");
    println!("\x1b[1;36m╠════════════════════════════════════════════╣\x1b[0m\r");
    println!("\x1b[1;36m║\x1b[0m      Microphone: {mic_icon} {mic_status}              \x1b[1;36m║\x1b[0m\r");
    println!("\x1b[1;36m╠════════════════════════════════════════════╣\x1b[0m\r");
    println!("\x1b[1;36m║\x1b[0m  SPACE = toggle mic                        \x1b[1;36m║\x1b[0m\r");
    println!("\x1b[1;36m║\x1b[0m  Q / ESC = hang up                         \x1b[1;36m║\x1b[0m\r");
    println!("\x1b[1;36m╚════════════════════════════════════════════╝\x1b[0m\r");

    io::stdout().flush().unwrap();
}
