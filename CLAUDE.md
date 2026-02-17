# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run Commands

```bash
cargo build --release          # Production build → ./target/release/hostelD
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

### System dependencies (Windows)

Visual Studio Build Tools ("Desktop development with C++") and CMake (added to PATH).

## Architecture

hostelD is a P2P encrypted voice + chat application over IPv6 UDP with no central server. Rust, ~2k LoC across 9 modules.

### Module responsibilities

- **`main.rs`** — CLI entry point, command routing (gui/tui/call/devices/mic-test)
- **`voice.rs`** — Core engine orchestrating the entire session: handshake, identity exchange, sender/receiver threads, audio streams, and chat message handling. All other modules are driven from here.
- **`crypto.rs`** — E2E encryption: X25519 key exchange → shared secret → ChaCha20-Poly1305 session cipher. Also handles local-storage encryption for chat history at rest.
- **`audio.rs`** — Cross-platform audio via cpal (ALSA/PipeWire on Linux, WASAPI on Windows). Mono 48kHz f32 samples through a lock-free ring buffer.
- **`identity.rs`** — Persistent X25519 keypair (`~/.hostelD/identity.key`), fingerprints (`hD-XXXXXXXX`), and JSON contact management.
- **`chat.rs`** — Encrypted chat history stored per-contact at `~/.hostelD/chats/{contact_id}.enc`.
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
4. Session key = SHA-256(shared_secret ‖ "hostelD-voice-key")
5. Verification code = SHA-256(shared_secret ‖ "hostelD-verify") → formatted XXXX-XXXX

### Local data layout

```
~/.hostelD/
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
