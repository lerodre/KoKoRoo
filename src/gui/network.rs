use eframe::egui;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Get the best (non-temporary, non-loopback) IPv6 address, optionally filtered by adapter.
pub(crate) fn get_best_ipv6(adapter: &str) -> String {
    let ifaces = get_network_interfaces();
    let filtered: Vec<&(String, String, String)> = if adapter.is_empty() {
        ifaces.iter().collect()
    } else {
        ifaces.iter().filter(|(iface, _, _)| iface == adapter).collect()
    };
    // Prefer global non-loopback
    for (_, ip, scope) in &filtered {
        if *ip != "::1" && scope == "global" {
            return ip.clone();
        }
    }
    // Fallback to link-local
    for (_, ip, _) in &filtered {
        if *ip != "::1" {
            return ip.clone();
        }
    }
    "::1".to_string()
}

/// Get unique network adapter names (excluding loopback and docker/veth).
pub(crate) fn get_adapter_names() -> Vec<String> {
    let ifaces = get_network_interfaces();
    let mut names: Vec<String> = Vec::new();
    for (iface, _, _) in &ifaces {
        if iface != "lo" && !names.contains(iface) {
            names.push(iface.clone());
        }
    }
    names
}

/// Returns (interface_name, ip, scope) for all non-temporary IPv6 addresses.
fn get_network_interfaces() -> Vec<(String, String, String)> {
    let mut result: Vec<(String, String, String)> = Vec::new();

    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = std::process::Command::new("ip").args(["-6", "addr", "show"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut current_iface = String::new();
            for line in text.lines() {
                let trimmed = line.trim();
                // Interface line: "3: wlp3s0: <BROADCAST,..."
                if !trimmed.starts_with("inet6") && trimmed.contains(": <") {
                    if let Some(name) = trimmed.split(':').nth(1) {
                        current_iface = name.trim().to_string();
                        // Strip @... suffix (e.g. "vethd3f93b1@enp2s0")
                        if let Some(pos) = current_iface.find('@') {
                            current_iface.truncate(pos);
                        }
                    }
                }
                if trimmed.starts_with("inet6") {
                    // Skip temporary privacy extension addresses
                    if trimmed.contains("temporary") {
                        continue;
                    }
                    if let Some(addr_cidr) = trimmed.split_whitespace().nth(1) {
                        let addr = addr_cidr.split('/').next().unwrap_or(addr_cidr);
                        if addr == "::1" { continue; }
                        let scope = if trimmed.contains("scope global") { "global" }
                            else if trimmed.contains("scope link") { "link-local" }
                            else { "other" };
                        result.push((current_iface.clone(), addr.to_string(), scope.to_string()));
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Use PowerShell CSV output to handle adapter names with spaces (e.g. "Ethernet 2", "vEthernet (WSL)")
        if let Ok(output) = std::process::Command::new("powershell")
            .args(["-Command", "Get-NetIPAddress -AddressFamily IPv6 | Where-Object { $_.SuffixOrigin -ne 'Random' -and $_.IPAddress -ne '::1' -and $_.AddressState -eq 'Preferred' } | Select-Object InterfaceAlias, IPAddress | ConvertTo-Csv -NoTypeInformation"])
            .creation_flags(0x08000000)
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines().skip(1) { // skip CSV header
                let line = line.trim();
                if line.is_empty() { continue; }
                // CSV format: "InterfaceAlias","IPAddress"
                let fields: Vec<&str> = line.split(',').collect();
                if fields.len() >= 2 {
                    let iface = fields[0].trim_matches('"').to_string();
                    let addr = fields[1].trim_matches('"').to_string();
                    if addr == "::1" || addr.is_empty() { continue; }
                    let scope = if addr.starts_with("fe80") { "link-local" } else { "global" };
                    result.push((iface, addr, scope.to_string()));
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("ifconfig").output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut current_iface = String::new();
            for line in text.lines() {
                // Interface header: "en0: flags=8863<UP,..."
                if !line.starts_with('\t') && !line.starts_with(' ') {
                    if let Some(name) = line.split(':').next() {
                        current_iface = name.to_string();
                    }
                }
                let trimmed = line.trim();
                if trimmed.starts_with("inet6") {
                    // Skip temporary/deprecated addresses
                    if trimmed.contains("deprecated") || trimmed.contains("temporary") {
                        continue;
                    }
                    // Format: "inet6 fe80::1%en0 prefixlen 64 scopeid 0x4"
                    // or:     "inet6 2001:db8::1 prefixlen 64"
                    if let Some(addr_raw) = trimmed.split_whitespace().nth(1) {
                        // Strip %interface suffix (e.g. "fe80::1%en0" → "fe80::1")
                        let addr = addr_raw.split('%').next().unwrap_or(addr_raw);
                        if addr == "::1" { continue; }
                        // Skip loopback interface
                        if current_iface == "lo0" { continue; }
                        let scope = if addr.starts_with("fe80") { "link-local" } else { "global" };
                        result.push((current_iface.clone(), addr.to_string(), scope.to_string()));
                    }
                }
            }
        }
    }

    // Sort: global first per interface
    result.sort_by_key(|(_, _, scope)| {
        match scope.as_str() {
            "global" => 0,
            "link-local" => 1,
            _ => 2,
        }
    });
    result
}

/// Format a contact/peer for display: "nickname #fingerprint" or just fingerprint.
pub(crate) fn format_peer_display(nickname: &str, fingerprint: &str) -> String {
    if nickname.is_empty() {
        fingerprint.to_string()
    } else {
        format!("{nickname} #{fingerprint}")
    }
}

/// Build a LayoutJob with nickname bold/large and #fingerprint smaller/gray.
pub(crate) fn peer_display_job(nickname: &str, fingerprint: &str, base_size: f32, name_color: egui::Color32, dim_color: egui::Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    if nickname.is_empty() {
        job.append(
            fingerprint,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(base_size),
                color: name_color,
                ..Default::default()
            },
        );
    } else {
        job.append(
            nickname,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(base_size + 1.0),
                color: name_color,
                ..Default::default()
            },
        );
        job.append(
            &format!(" #{fingerprint}"),
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(base_size - 2.0),
                color: dim_color,
                ..Default::default()
            },
        );
    }
    job
}

/// Censor an IP address: show first group, mask the rest.
/// e.g. "2803:c600:d310:..." → "2803:****"
pub(crate) fn censor_ip(ip: &str) -> String {
    if ip == "::1" || ip.is_empty() {
        return ip.to_string();
    }
    // Find the first ':' and keep everything before it
    if let Some(pos) = ip.find(':') {
        format!("{}:****", &ip[..pos])
    } else if let Some(pos) = ip.find('.') {
        // IPv4: show first octet
        format!("{}.***.***", &ip[..pos])
    } else {
        "****".to_string()
    }
}
