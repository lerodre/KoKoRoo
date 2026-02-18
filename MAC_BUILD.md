# macOS Build Guide (Intel + Apple Silicon)

## Prerequisites

### Xcode Command Line Tools (required)

```bash
xcode-select --install
```

### Homebrew packages

```bash
brew install opus cmake yasm pkg-config
```

## Build

```bash
# Native build (whatever arch you're on)
cargo build --release

# Intel only
rustup target add x86_64-apple-darwin
cargo build --release --target x86_64-apple-darwin

# Apple Silicon only
rustup target add aarch64-apple-darwin
cargo build --release --target aarch64-apple-darwin

# Universal binary (both archs in one binary)
lipo -create \
    target/x86_64-apple-darwin/release/hostelD \
    target/aarch64-apple-darwin/release/hostelD \
    -output hostelD-universal
```

## Platform differences from Linux

### Already works on macOS (no changes needed)

- **Audio** (cpal) — uses CoreAudio natively
- **GUI** (eframe/egui) — native macOS windowing via winit
- **Screen capture** (scrap) — uses CGDisplayStream
- **Crypto** — pure Rust, no platform deps
- **Noise suppression** (nnnoiseless) — pure Rust

### Needs attention

| Area | Status | Notes |
|------|--------|-------|
| `sysfirewall.rs` | Fallback exists | macOS app firewall is application-based, not port-based. The existing fallback returns an error message and continues — functionally fine. Add a `#[cfg(target_os = "macos")]` block that returns `Ok(false)` for cleaner behavior. |
| `get_ipv6_addresses()` | Missing macOS impl | Currently only parses `ip -6 addr show` (Linux) and PowerShell (Windows). Needs `ifconfig` parsing for macOS. |
| `sysaudio.rs` (system audio capture) | Disabled by fallback | macOS blocks loopback capture by default. Requires a virtual audio driver (BlackHole or Soundflower) installed by the user. The app can detect these as input devices. |
| `nokhwa` (webcam) | Untested | v0.10 has experimental AVFoundation support. May compile and work, or may need `#[cfg]` disable on macOS. |
| `env-libvpx-sys` | Should compile | The `"generate"` feature builds libvpx from source using cmake/yasm. Needs testing — may need flag adjustments for macOS. |
| `wayland_capture.rs` | N/A | Already `#[cfg(target_os = "linux")]` only. |

### Code changes required

1. **`src/gui/mod.rs`** — Add `#[cfg(target_os = "macos")]` block in `get_ipv6_addresses()`:
   ```rust
   #[cfg(target_os = "macos")]
   {
       if let Ok(output) = std::process::Command::new("ifconfig").output() {
           // parse "inet6 ..." lines
       }
   }
   ```

2. **`src/sysfirewall.rs`** — Add macOS handler:
   ```rust
   #[cfg(target_os = "macos")]
   fn ensure_macos(_port: u16) -> Result<bool, String> {
       Ok(false) // macOS app firewall doesn't block by port
   }
   ```

3. **`src/sysaudio.rs`** — Optionally add macOS support:
   ```rust
   #[cfg(target_os = "macos")]
   {
       // Look for BlackHole/Soundflower input devices
       // or return None with a log message
   }
   ```

4. **`Cargo.toml`** — May need conditional features for `env-libvpx-sys` and `nokhwa` depending on test results.

## Troubleshooting

### libopus not found
```bash
export PKG_CONFIG_PATH="/opt/homebrew/lib/pkgconfig:$PKG_CONFIG_PATH"  # Apple Silicon
# or
export PKG_CONFIG_PATH="/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH"     # Intel
```

### libvpx build fails
```bash
brew install nasm  # alternative to yasm
```

### Screen capture permission
macOS requires Screen Recording permission. The OS will prompt on first use. Grant it in System Settings > Privacy & Security > Screen Recording.

### Microphone permission
macOS requires Microphone permission. Grant it in System Settings > Privacy & Security > Microphone.

## Distribution

### Option 1: Raw binary
Just ship `target/release/hostelD` (or the universal binary). Users run it from terminal.

### Option 2: .app bundle
Use `cargo-bundle` or manually create:
```
hostelD.app/
  Contents/
    Info.plist
    MacOS/
      hostelD          # the binary
    Resources/
      hostelD.icns     # app icon
```

### Code signing (optional but recommended)
```bash
codesign --sign "Developer ID Application: ..." hostelD.app
```
Without signing, users must right-click > Open to bypass Gatekeeper on first launch.
