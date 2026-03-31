# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run Commands

```bash
cargo build --release          # Production build → ./target/release/kokoroo
cargo build                    # Debug build
cargo run                      # Run GUI (default mode)
cargo run -- tui               # Terminal UI
cargo run -- call <peer-ipv6> <peer-port> <local-port>  # Direct CLI call
cargo run -- devices           # List audio devices
cargo run -- mic-test          # Audio loopback test
```

### System dependencies (Linux)

```bash
sudo apt install build-essential pkg-config cmake libasound2-dev libopus-dev \
    libxkbcommon-dev libgtk-3-dev libgl1-mesa-dev libegl1-mesa-dev
```

### System dependencies (macOS)

```bash
xcode-select --install
brew install opus cmake yasm pkg-config
```

If libopus is not found during build:
```bash
# Apple Silicon
export PKG_CONFIG_PATH="/opt/homebrew/lib/pkgconfig:$PKG_CONFIG_PATH"
# Intel
export PKG_CONFIG_PATH="/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH"
```

Build: `cp Cargo.toml.macos Cargo.toml && cargo build --release`

### System dependencies (Windows)

Visual Studio Build Tools ("Desktop development with C++") and CMake (added to PATH).

## Per-platform Cargo.toml

The `Cargo.toml` file is `.gitignore`d and managed per-OS. Copy the right variant before building:

- **Linux**: `cp Cargo.toml.linux Cargo.toml`
- **macOS**: `cp Cargo.toml.macos Cargo.toml`
- **Windows**: `copy Cargo.toml.windows Cargo.toml`

### Platform-specific dependencies

| Dependency | Linux | macOS | Windows | Notes |
|-----------|-------|-------|---------|-------|
| `cpal` | ALSA/PipeWire | CoreAudio | WASAPI | Audio I/O |
| `nokhwa` | V4L2 | AVFoundation | MediaFoundation | Webcam |
| `pipewire`/`libspa`/`ashpd` | yes | excluded | excluded | Wayland screen capture |
| `scrap` | X11/DXGI | CGDisplayStream | DXGI | Screen capture |
| `winresource` | excluded | excluded | yes | Windows icon embedding |

### macOS-specific notes

- **Firewall**: macOS uses app-based firewall (not port-based). The app returns `Ok(false)` and lets macOS prompt the user.
- **IPv6 discovery**: Uses `ifconfig` output parsing (Linux uses `ip -6 addr show`, Windows uses PowerShell).
- **Ringtone**: Uses `afplay` (Linux uses `gst-play-1.0`, Windows uses `winmm.dll`).
- **Notifications**: Uses `osascript` AppleScript (Linux uses `notify-send`, Windows uses PowerShell).
- **System audio capture**: Uses ScreenCaptureKit (macOS 13+) for native loopback — no virtual audio drivers needed. Requires Screen Recording permission.
- **Webcam**: Uses `nokhwa` with AVFoundation backend. Requires Camera permission.
- **Screen capture**: Works natively via `scrap` (CGDisplayStream). Requires Screen Recording permission.
- **Permissions**: macOS will prompt for Microphone, Camera, and Screen Recording permissions on first use. Grant in System Settings > Privacy & Security. After rebuilding, permissions may need to be re-granted (`tccutil reset ScreenCapture`).

## Architecture

KoKoRoo is a P2P encrypted voice + chat application over IPv6 UDP with no central server. Rust, ~2k LoC across 9 modules.

### Module responsibilities

- **`main.rs`** — CLI entry point, command routing (gui/tui/call/devices/mic-test)
- **`voice.rs`** — Core engine orchestrating the entire session: handshake, identity exchange, sender/receiver threads, audio streams, and chat message handling. All other modules are driven from here.
- **`crypto.rs`** — E2E encryption: X25519 key exchange → shared secret → ChaCha20-Poly1305 session cipher. Also handles local-storage encryption for chat history at rest.
- **`audio.rs`** — Cross-platform audio via cpal (ALSA/PipeWire on Linux, CoreAudio on macOS, WASAPI on Windows). Mono 48kHz f32 samples through a lock-free ring buffer.
- **`identity.rs`** — Persistent X25519 keypair (`~/.kokoroo/identity.key`), fingerprints (`KR-XXXXXXXX`), and JSON contact management.
- **`chat.rs`** — Encrypted chat history stored per-contact at `~/.kokoroo/chats/{contact_id}.enc`.
- **`firewall.rs`** — Per-IP rate limiting (>200 pkt/sec = strike) and auto-blacklist (5 strikes).
- **`gui.rs`** — Desktop GUI with eframe/egui. State machine: Setup → Connecting → InCall → Error.
- **`ui.rs`** — Terminal UI with crossterm. Arrow-key menus, text input, live call screen.

### Threading model (voice.rs)

A call spawns 4 concurrent paths:
1. **Mic callback** (cpal input stream) → pushes f32 samples into ring buffer
2. **Sender thread** → reads ring buffer → Opus encode → encrypt → UDP send (also sends outgoing chat)
3. **Receiver thread** → UDP recv → firewall check → decrypt → Opus decode → pushes to speaker ring buffer (also receives chat)
4. **Speaker callback** (cpal output stream) → pulls from ring buffer

### Packet protocol (UDP)

| Type byte | Name     | Payload                                          |
|-----------|----------|--------------------------------------------------|
| `0x01`    | HELLO    | 32-byte ephemeral X25519 pubkey                  |
| `0x02`    | VOICE    | 4-byte counter + Opus ciphertext + 16B tag       |
| `0x03`    | IDENTITY | Encrypted: 32-byte identity pubkey + nickname    |
| `0x04`    | CHAT     | Encrypted: text message                          |
| `0x05`    | HANGUP   | Encrypted: empty payload (mutual disconnect)     |

### Key exchange flow

1. Both peers generate ephemeral X25519 keypairs
2. Exchange HELLO packets (retry up to 60×500ms)
3. X25519 DH → shared secret
4. Session key = SHA-256(shared_secret ‖ "kokoroo-voice-key")
5. Verification code = SHA-256(shared_secret ‖ "kokoroo-verify") → formatted XXXX-XXXX

### Local data layout

```
~/.kokoroo/
├── identity.key              # 64 bytes: 32 secret + 32 public (0600 perms on Unix)
├── settings.json             # Nickname, mic, speakers, port preferences
├── contacts/{pubkey_hex}.json  # One file per peer (64-char hex = collision-proof)
└── chats/{contact_id}.enc    # ChaCha20-Poly1305 encrypted JSON
```

Contact files are keyed by full public key hex (not fingerprint) to avoid collisions. Contact IDs are deterministic: SHA-256(sorted(pubkey_A, pubkey_B)), so both peers derive the same ID.

### Trust model (TOFU)

- First connection: trust the peer's key + nickname, save contact
- Subsequent connections: verify same key, increment `call_count`
- Nickname claimed by different key → WARNING (possible impersonation)
- Unknown key from same address as known contact → WARNING
- Nicknames never inherited across different public keys

### Audio pipeline

Opus codec: 48kHz mono, 960-sample frames (20ms), 64kbps bitrate, VOIP application mode.

## Git conventions

- **No credits/co-author lines** in commit messages. Never add `Co-Authored-By` or similar attribution tags.
- Commit messages should be concise and describe what changed and why.

## Branch & release workflow

- **`master`** is the only permanent branch. All work merges back to it.
- **Fixes**: `fix/<short-description>` (e.g. `fix/handshake-loop`). For bug fixes.
- **Features**: `feat/<short-description>` (e.g. `feat/delete-contact`). For new functionality.
- Branches are deleted after merging. No long-lived branches.
- **Don't release per-fix.** Accumulate fixes and features, then cut a single release.
- Track pending changes in `CHANGELOG.unreleased.md`. When releasing, move its contents to the GitHub release notes and clear the file.
- **Versioning**: semver. `v0.1.1` for patches/small features, `v0.2.0` for major features.
