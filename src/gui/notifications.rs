use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Start playing the ringtone in a loop on a background thread.
/// Returns a stop flag; set it to true to stop playback.
pub(crate) fn start_ringtone() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    std::thread::spawn(move || {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let path = format!("{}/.kokoroo/ringtone.mp3", std::env::var("HOME").unwrap_or_default());
        #[cfg(target_os = "windows")]
        let path = format!("{}\\.kokoroo\\ringtone.mp3", std::env::var("USERPROFILE").unwrap_or_default());

        if !std::path::Path::new(&path).exists() {
            return;
        }

        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            #[cfg(target_os = "linux")]
            let mut child = match std::process::Command::new("gst-play-1.0")
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => break,
            };

            #[cfg(target_os = "windows")]
            let mut child = match std::process::Command::new("powershell")
                .args([
                    "-WindowStyle", "Hidden", "-Command",
                    &format!(
                        "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; \
                         public class WinMM {{ [DllImport(\"winmm.dll\")] \
                         public static extern int mciSendString(string cmd, System.Text.StringBuilder buf, int sz, IntPtr cb); }}'; \
                         $null=[WinMM]::mciSendString('open \"{}\" alias hostelring', $null, 0, [IntPtr]::Zero); \
                         $null=[WinMM]::mciSendString('play hostelring wait', $null, 0, [IntPtr]::Zero); \
                         $null=[WinMM]::mciSendString('close hostelring', $null, 0, [IntPtr]::Zero)",
                        path.replace('\\', "/")
                    ),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(0x08000000)
                .spawn()
            {
                Ok(c) => c,
                Err(_) => break,
            };

            #[cfg(target_os = "macos")]
            let mut child = match std::process::Command::new("afplay")
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => break,
            };

            loop {
                if stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                    child.kill().ok();
                    child.wait().ok();
                    return;
                }
                match child.try_wait() {
                    Ok(Some(_)) => break, // Finished playing, loop again
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                    Err(_) => return,
                }
            }
        }
    });
    stop
}

/// Play the notification sound once in a background thread.
pub(crate) fn play_notification_sound() {
    std::thread::spawn(|| {
        #[cfg(target_os = "linux")]
        let home = std::env::var("HOME").unwrap_or_default();
        #[cfg(target_os = "windows")]
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        #[cfg(target_os = "macos")]
        let home = std::env::var("HOME").unwrap_or_default();

        let sep = if cfg!(windows) { "\\" } else { "/" };
        let path = format!("{home}{sep}.kokoroo{sep}notification.mp3");

        if !std::path::Path::new(&path).exists() {
            return;
        }

        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("gst-play-1.0")
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn().ok();
        }

        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("powershell")
                .args([
                    "-WindowStyle", "Hidden", "-Command",
                    &format!(
                        "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; \
                         public class WinMM {{ [DllImport(\"winmm.dll\")] \
                         public static extern int mciSendString(string cmd, System.Text.StringBuilder buf, int sz, IntPtr cb); }}'; \
                         $null=[WinMM]::mciSendString('open \"{}\" alias hostelnotif', $null, 0, [IntPtr]::Zero); \
                         $null=[WinMM]::mciSendString('play hostelnotif wait', $null, 0, [IntPtr]::Zero); \
                         $null=[WinMM]::mciSendString('close hostelnotif', $null, 0, [IntPtr]::Zero)",
                        path.replace('\\', "/")
                    ),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(0x08000000)
                .spawn().ok();
        }

        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("afplay")
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn().ok();
        }
    });
}

/// Write the embedded notification sound to ~/.kokoroo/ if it doesn't exist yet.
pub(crate) fn ensure_notification_sound() {
    #[cfg(target_os = "linux")]
    let home = std::env::var("HOME").unwrap_or_default();
    #[cfg(target_os = "windows")]
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    #[cfg(target_os = "macos")]
    let home = std::env::var("HOME").unwrap_or_default();

    let sep = if cfg!(windows) { "\\" } else { "/" };
    let path = format!("{home}{sep}.kokoroo{sep}notification.mp3");

    if !std::path::Path::new(&path).exists() {
        let bytes = include_bytes!("../../assets/notification.mp3");
        std::fs::write(&path, bytes).ok();
    }
}

/// Send an OS-level desktop notification (notify-send on Linux, PowerShell balloon on Windows).
pub(crate) fn send_desktop_notification(title: &str, body: &str) {
    let t = title.to_string();
    let b = body.to_string();
    std::thread::spawn(move || {
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("notify-send")
                .args(["-u", "critical", "-a", "KoKoRoo", &t, &b])
                .spawn()
                .ok();
        }
        #[cfg(target_os = "windows")]
        {
            let script = format!(
                "[void] [System.Reflection.Assembly]::LoadWithPartialName('System.Windows.Forms'); \
                 $n = New-Object System.Windows.Forms.NotifyIcon; \
                 $n.Icon = [System.Drawing.SystemIcons]::Information; \
                 $n.Visible = $true; \
                 $n.ShowBalloonTip(5000, '{}', '{}', 'Info'); \
                 Start-Sleep -Seconds 6; $n.Dispose()",
                t.replace('\'', "''"),
                b.replace('\'', "''"),
            );
            std::process::Command::new("powershell")
                .args(["-WindowStyle", "Hidden", "-Command", &script])
                .creation_flags(0x08000000)
                .spawn()
                .ok();
        }
        #[cfg(target_os = "macos")]
        {
            let script = format!(
                "display notification \"{}\" with title \"{}\"",
                b.replace('\\', "\\\\").replace('"', "\\\""),
                t.replace('\\', "\\\\").replace('"', "\\\""),
            );
            std::process::Command::new("osascript")
                .args(["-e", &script])
                .spawn()
                .ok();
        }
    });
}
