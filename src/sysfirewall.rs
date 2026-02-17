use std::process::Command;

/// Ensure a UDP firewall rule exists for the given port.
/// Returns `Ok(true)` if a rule was added, `Ok(false)` if it already existed,
/// or `Err` if the operation failed (never fatal — callers should log and continue).
pub fn ensure_udp_port_open(port: u16) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    return ensure_windows(port);

    #[cfg(target_os = "linux")]
    return ensure_linux(port);

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = port;
        Err("Automatic firewall rules not supported on this OS".into())
    }
}

#[cfg(target_os = "windows")]
fn ensure_windows(port: u16) -> Result<bool, String> {
    let rule_name = format!("hostelD UDP {port}");

    // Check if rule already exists
    let check = Command::new("netsh")
        .args(["advfirewall", "firewall", "show", "rule", &format!("name={rule_name}")])
        .output()
        .map_err(|e| format!("Failed to run netsh: {e}"))?;

    let stdout = String::from_utf8_lossy(&check.stdout);
    if stdout.contains(&rule_name) {
        return Ok(false); // already exists
    }

    // Add rule via PowerShell Start-Process -Verb RunAs (triggers UAC)
    let netsh_cmd = format!(
        "netsh advfirewall firewall add rule name=\\\"{rule_name}\\\" dir=in action=allow protocol=UDP localport={port}"
    );
    let add = Command::new("powershell")
        .args([
            "-Command",
            &format!("Start-Process -FilePath 'cmd.exe' -ArgumentList '/c {netsh_cmd}' -Verb RunAs -Wait"),
        ])
        .output()
        .map_err(|e| format!("Failed to launch UAC prompt: {e}"))?;

    if !add.status.success() {
        let stderr = String::from_utf8_lossy(&add.stderr);
        return Err(format!("UAC/netsh failed: {stderr}"));
    }

    // Verify the rule was actually added
    let verify = Command::new("netsh")
        .args(["advfirewall", "firewall", "show", "rule", &format!("name={rule_name}")])
        .output()
        .map_err(|e| format!("Failed to verify rule: {e}"))?;

    let verify_out = String::from_utf8_lossy(&verify.stdout);
    if verify_out.contains(&rule_name) {
        Ok(true)
    } else {
        Err("Rule was not added (user may have declined UAC)".into())
    }
}

#[cfg(target_os = "linux")]
fn ensure_linux(port: u16) -> Result<bool, String> {
    // Check if ufw is available and active
    let status = Command::new("ufw")
        .arg("status")
        .output();

    let status = match status {
        Ok(s) => s,
        Err(_) => return Err("ufw not found — skipping firewall rule".into()),
    };

    let stdout = String::from_utf8_lossy(&status.stdout);

    // If ufw is inactive, skip
    if stdout.contains("inactive") {
        return Err("ufw is inactive — skipping firewall rule".into());
    }

    // Check if rule already exists
    let port_pattern = format!("{port}/udp");
    if stdout.contains(&port_pattern) {
        return Ok(false); // already exists
    }

    // Add rule via pkexec (graphical sudo prompt)
    let add = Command::new("pkexec")
        .args(["ufw", "allow", &port_pattern])
        .output()
        .map_err(|e| format!("Failed to run pkexec ufw: {e}"))?;

    if add.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&add.stderr);
        Err(format!("pkexec ufw failed: {stderr}"))
    }
}
