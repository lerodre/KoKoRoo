// Hide the console window on Windows release builds (GUI app).
// The console still appears if launched from a terminal with arguments (tui, call, etc).
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::env;
use std::net::UdpSocket;

#[macro_use]
mod logger;
mod audio;
mod chat;
mod crypto;
mod firewall;
mod gui;
mod identity;
pub mod theme;
mod messaging;
mod screen;
mod sysaudio;
mod sysfirewall;
mod voice;
mod ui;
#[cfg(target_os = "linux")]
mod wayland_capture;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        // No args → launch GUI
        gui::run();
        return;
    }

    match args[1].as_str() {
        "gui" => {
            gui::run();
        }
        "tui" => {
            ui::run();
        }
        "call" => {
            if args.len() < 5 {
                eprintln!("Usage: hostelD call <peer-ip> <peer-port> <local-port>");
                eprintln!("Example: hostelD call ::1 9000 9001");
                return;
            }
            voice::call(&args[2], &args[3], &args[4]);
        }
        "listen" => {
            let port = args.get(2).map(|s| s.as_str()).unwrap_or("9000");
            listen(port);
        }
        "send" => {
            if args.len() < 5 {
                eprintln!("Usage: hostelD send <ip> <port> <message>");
                return;
            }
            send(&args[2], &args[3], &args[4]);
        }
        "devices" => {
            audio::list_devices();
        }
        "mic-test" => {
            audio::mic_test();
        }
        other => {
            eprintln!("Unknown command: {other}");
        }
    }
}

/// Binds to [::]:port and listens for incoming UDP packets. Echoes them back.
fn listen(port: &str) {
    let bind_addr = format!("[::]:{port}");

    let socket = UdpSocket::bind(&bind_addr).unwrap_or_else(|e| {
        eprintln!("Failed to bind to {bind_addr}: {e}");
        std::process::exit(1);
    });

    println!("Listening on {bind_addr} (IPv6 UDP)");
    println!("Waiting for messages... (Ctrl+C to stop)");

    let mut buf = [0u8; 1500];

    loop {
        match socket.recv_from(&mut buf) {
            Ok((bytes_read, sender_addr)) => {
                let message = String::from_utf8_lossy(&buf[..bytes_read]);
                println!("[{sender_addr}] ({bytes_read} bytes): {message}");

                let reply = format!("echo: {message}");
                if let Err(e) = socket.send_to(reply.as_bytes(), sender_addr) {
                    eprintln!("Failed to echo back to {sender_addr}: {e}");
                }
            }
            Err(e) => {
                eprintln!("Receive error: {e}");
            }
        }
    }
}

/// Sends a UDP message to a peer and waits for an echo reply.
fn send(ip: &str, port: &str, message: &str) {
    let target = format!("[{ip}]:{port}");

    let socket = UdpSocket::bind("[::]:0").unwrap_or_else(|e| {
        eprintln!("Failed to bind socket: {e}");
        std::process::exit(1);
    });

    let local_addr = socket.local_addr().unwrap();
    println!("Sending from {local_addr}");
    println!("Target: {target}");
    println!("Message: \"{message}\"");

    match socket.send_to(message.as_bytes(), &target) {
        Ok(bytes_sent) => {
            println!("Sent {bytes_sent} bytes");
        }
        Err(e) => {
            eprintln!("Send failed: {e}");
            return;
        }
    }

    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();

    let mut buf = [0u8; 1500];
    match socket.recv_from(&mut buf) {
        Ok((bytes_read, from)) => {
            let reply = String::from_utf8_lossy(&buf[..bytes_read]);
            println!("Reply from {from}: {reply}");
        }
        Err(_) => {
            println!("No reply received (timeout).");
        }
    }
}
