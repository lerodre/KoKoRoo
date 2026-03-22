#[cfg(any(target_os = "windows", target_os = "linux"))]
use std::process::Command;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Remove the KoKoRoo UDP firewall rule for the given port.
/// Returns `Ok(true)` if a rule was removed, `Ok(false)` if none existed,
/// or `Err` if the operation failed.
pub fn remove_udp_port_rule(port: u16) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    return remove_windows(port);

    #[cfg(target_os = "linux")]
    return remove_linux(port);

    #[cfg(target_os = "macos")]
    {
        let _ = port;
        // macOS uses an application-based firewall, not port-based rules.
        // No port rule to remove.
        Ok(false)
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = port;
        Err("Automatic firewall rules not supported on this OS".into())
    }
}

/// Ensure a UDP firewall rule exists for the given port.
/// Returns `Ok(true)` if a rule was added, `Ok(false)` if it already existed,
/// or `Err` if the operation failed (never fatal — callers should log and continue).
pub fn ensure_udp_port_open(port: u16) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    return ensure_windows(port);

    #[cfg(target_os = "linux")]
    return ensure_linux(port);

    #[cfg(target_os = "macos")]
    {
        let _ = port;
        // macOS uses an application-based firewall (not port-based).
        // The OS will prompt the user to allow network access on first launch.
        Ok(false)
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = port;
        Err("Automatic firewall rules not supported on this OS".into())
    }
}

#[cfg(target_os = "windows")]
fn ensure_windows(port: u16) -> Result<bool, String> {
    let rule_name = format!("KoKoRoo UDP {port}");

    // Check if rule already exists
    let check = Command::new("netsh")
        .args(["advfirewall", "firewall", "show", "rule", &format!("name={rule_name}")])
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| format!("Failed to run netsh: {e}"))?;

    let stdout = String::from_utf8_lossy(&check.stdout);
    if stdout.contains(&rule_name) {
        return Ok(false); // already exists
    }

    // Get the path to this executable so the rule only applies to KoKoRoo
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("Failed to get exe path: {e}"))?;
    let exe_str = exe_path.to_string_lossy().replace('\\', "\\\\");

    // Add rule via PowerShell Start-Process -Verb RunAs (triggers UAC)
    // Restrictions: inbound only, UDP, IPv6 only, this program only
    let netsh_cmd = format!(
        "netsh advfirewall firewall add rule name=\\\"{rule_name}\\\" dir=in action=allow protocol=UDP localport={port} program=\\\"{exe_str}\\\" remoteip=any localip=any profile=any interfacetype=any"
    );
    let add = Command::new("powershell")
        .args([
            "-Command",
            &format!("Start-Process -FilePath 'cmd.exe' -ArgumentList '/c {netsh_cmd}' -Verb RunAs -Wait"),
        ])
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| format!("Failed to launch UAC prompt: {e}"))?;

    if !add.status.success() {
        let stderr = String::from_utf8_lossy(&add.stderr);
        return Err(format!("UAC/netsh failed: {stderr}"));
    }

    // Verify the rule was actually added
    let verify = Command::new("netsh")
        .args(["advfirewall", "firewall", "show", "rule", &format!("name={rule_name}")])
        .creation_flags(0x08000000)
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

    // Add rule via pkexec — IPv6 only, inbound UDP, with comment for identification
    let add = Command::new("pkexec")
        .args(["ufw", "allow", "in", "proto", "udp", "to", "any", "port", &port.to_string(),
               "comment", &format!("KoKoRoo UDP {port}")])
        .output()
        .map_err(|e| format!("Failed to run pkexec ufw: {e}"))?;

    if add.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&add.stderr);
        Err(format!("pkexec ufw failed: {stderr}"))
    }
}

#[cfg(target_os = "windows")]
fn remove_windows(port: u16) -> Result<bool, String> {
    let rule_name = format!("KoKoRoo UDP {port}");

    // Check if rule exists
    let check = Command::new("netsh")
        .args(["advfirewall", "firewall", "show", "rule", &format!("name={rule_name}")])
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| format!("Failed to run netsh: {e}"))?;

    let stdout = String::from_utf8_lossy(&check.stdout);
    if !stdout.contains(&rule_name) {
        return Ok(false); // nothing to remove
    }

    // Delete rule via PowerShell Start-Process -Verb RunAs (triggers UAC)
    let netsh_cmd = format!(
        "netsh advfirewall firewall delete rule name=\\\"{rule_name}\\\""
    );
    let del = Command::new("powershell")
        .args([
            "-Command",
            &format!("Start-Process -FilePath 'cmd.exe' -ArgumentList '/c {netsh_cmd}' -Verb RunAs -Wait"),
        ])
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| format!("Failed to launch UAC prompt: {e}"))?;

    if !del.status.success() {
        let stderr = String::from_utf8_lossy(&del.stderr);
        return Err(format!("UAC/netsh delete failed: {stderr}"));
    }

    Ok(true)
}

#[cfg(target_os = "linux")]
fn remove_linux(port: u16) -> Result<bool, String> {
    let port_pattern = format!("{port}/udp");

    // Check if rule exists
    let status = Command::new("ufw")
        .arg("status")
        .output()
        .map_err(|e| format!("Failed to run ufw: {e}"))?;

    let stdout = String::from_utf8_lossy(&status.stdout);
    if !stdout.contains(&port_pattern) {
        return Ok(false); // nothing to remove
    }

    // Delete rule via pkexec (matches the format used in ensure_linux)
    let del = Command::new("pkexec")
        .args(["ufw", "delete", "allow", "in", "proto", "udp", "to", "any", "port", &port.to_string()])
        .output()
        .map_err(|e| format!("Failed to run pkexec ufw: {e}"))?;

    if del.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&del.stderr);
        Err(format!("pkexec ufw delete failed: {stderr}"))
    }
}
