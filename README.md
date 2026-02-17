# hostelD

P2P encrypted voice + chat + screen sharing over IPv6. No servers, no accounts, no registration.

```
 ┌──────────┐       UDP/IPv6        ┌──────────┐
 │  Peer A  │ <───────────────────> │  Peer B  │
 │          │   E2E Encrypted       │          │
 │ identity │   Voice + Chat +      │ identity │
 │  .key    │   Screen Share        │  .key    │
 └──────────┘                       └──────────┘
```

## Features

- **Voice calls** — Opus 64kbps, full duplex, RNNoise noise suppression
- **E2E encryption** — X25519 key exchange + ChaCha20-Poly1305 (same primitives as Signal/WireGuard)
- **Real-time chat** — encrypted in transit and at rest
- **Screen sharing** — VP8 encoded, multi-monitor selection, system audio capture
- **Persistent identity** — `hD-XXXXXXXX` fingerprints, TOFU trust model, contact management
- **Anti-spam firewall** — per-IP rate limiting + auto-blacklist
- **Desktop GUI** (eframe/egui) and **Terminal UI** (crossterm)
- **LAN & Internet modes** — IPv6 link-local or global

---

## Setup — Windows

### 1. Install Rust

Download and run the installer from [rustup.rs](https://rustup.rs). Accept the defaults (MSVC toolchain).

### 2. Install Visual Studio Build Tools

Download from [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/).

Select **"Desktop development with C++"** (installs MSVC compiler + Windows SDK).

### 3. Install CMake

Download from [cmake.org](https://cmake.org/download/). Select **"Add CMake to the system PATH"** during install.

### 4. Install libvpx (for screen sharing)

Using [vcpkg](https://github.com/microsoft/vcpkg):

```powershell
git clone https://github.com/microsoft/vcpkg.git C:\vcpkg
C:\vcpkg\bootstrap-vcpkg.bat
C:\vcpkg\vcpkg install libvpx:x64-windows
```

Then set the environment variables (PowerShell as admin):

```powershell
[System.Environment]::SetEnvironmentVariable('VPX_LIB_DIR', 'C:\vcpkg\installed\x64-windows\lib', 'User')
[System.Environment]::SetEnvironmentVariable('VPX_INCLUDE_DIR', 'C:\vcpkg\installed\x64-windows\include', 'User')
[System.Environment]::SetEnvironmentVariable('VPX_VERSION', '1.15.2', 'User')
```

Restart your terminal after setting these.

### 5. Clone and build

```powershell
git clone https://github.com/lerodre/hostelD.git
cd hostelD
cargo build --release
```

Binary: `.\target\release\hostelD.exe`

### 6. Run

```powershell
.\target\release\hostelD.exe            # GUI (default)
.\target\release\hostelD.exe tui        # Terminal UI
.\target\release\hostelD.exe call <peer-ipv6> <peer-port> <local-port>
.\target\release\hostelD.exe devices    # List audio devices
```

### Windows audio note

`cpal` uses WASAPI (built-in, no extra drivers). System audio capture for screen sharing uses WASAPI loopback.

---

## Setup — macOS

### 1. Install Xcode Command Line Tools

```bash
xcode-select --install
```

### 2. Install Homebrew dependencies

```bash
brew install cmake opus libvpx pkg-config
```

### 3. Set libvpx environment variables

```bash
# Add to ~/.zshrc or ~/.bash_profile:
export VPX_LIB_DIR="$(brew --prefix libvpx)/lib"
export VPX_INCLUDE_DIR="$(brew --prefix libvpx)/include"
export VPX_VERSION="$(brew info libvpx --json | python3 -c 'import sys,json; print(json.load(sys.stdin)[0][\"versions\"][\"stable\"])')"
```

Reload your shell: `source ~/.zshrc`

### 4. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### 5. Clone and build

```bash
git clone https://github.com/lerodre/hostelD.git
cd hostelD
cargo build --release
```

Binary: `./target/release/hostelD`

### 6. Run

```bash
./target/release/hostelD                # GUI (default)
./target/release/hostelD tui            # Terminal UI
./target/release/hostelD call <peer-ipv6> <peer-port> <local-port>
```

### macOS notes

- Audio uses CoreAudio via `cpal` (built-in)
- Screen capture may require **Screen Recording** permission in System Settings > Privacy & Security
- macOS has IPv6 enabled by default on most networks

---

## Setup — Linux (Ubuntu/Debian)

### 1. Install system dependencies

```bash
sudo apt update
sudo apt install -y build-essential pkg-config cmake \
    libasound2-dev libopus-dev libvpx-dev \
    libxkbcommon-dev libgtk-3-dev \
    libgl1-mesa-dev libegl1-mesa-dev
```

| Package | Why |
|---------|-----|
| `build-essential` | C compiler (gcc) for native crates |
| `pkg-config` | Finds system libraries at build time |
| `cmake` | Builds libopus from source (audiopus_sys) |
| `libasound2-dev` | ALSA headers — audio backend for `cpal` |
| `libopus-dev` | Opus codec library |
| `libvpx-dev` | VP8/VP9 codec for screen sharing |
| `libxkbcommon-dev` | Keyboard input for GUI (winit/egui) |
| `libgtk-3-dev` | Native dialogs (egui) |
| `libgl1-mesa-dev` | OpenGL headers for GUI rendering |
| `libegl1-mesa-dev` | EGL for OpenGL context creation |

### 2. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### 3. Clone and build

```bash
git clone https://github.com/lerodre/hostelD.git
cd hostelD
cargo build --release
```

Binary: `./target/release/hostelD`

### 4. Run

```bash
./target/release/hostelD                # GUI (default)
./target/release/hostelD tui            # Terminal UI
./target/release/hostelD call <peer-ipv6> <peer-port> <local-port>
./target/release/hostelD devices        # List audio devices
./target/release/hostelD mic-test       # Audio loopback test
```

### PipeWire / PulseAudio note

On modern Ubuntu (22.04+), audio runs through PipeWire with ALSA compatibility. `cpal` uses ALSA by default, which works transparently. No extra config needed.

---

## Testing on LAN

### 1. Find your IPv6 addresses

```bash
# Linux
ip -6 addr show | grep "scope link"

# Windows
ipconfig    # Look for "Link-local IPv6 Address"

# macOS
ifconfig | grep inet6
```

### 2. Verify connectivity

```bash
# Linux/macOS
ping6 fe80::PEER_ADDRESS%eth0

# Windows
ping fe80::PEER_ADDRESS%Ethernet
```

Replace `eth0`/`Ethernet` with your actual interface name.

### 3. Firewall

hostelD automatically creates Windows Firewall rules when starting a call. For manual setup:

```powershell
# Windows (Admin)
netsh advfirewall firewall add rule name="hostelD" dir=in action=allow protocol=UDP localport=9000-9100
```

```bash
# Linux
sudo ufw allow 9000:9100/udp
```

### 4. Start the call

**PC 1 (port 9000):**
```bash
./hostelD call fe80::PC2_ADDRESS%eth0 9001 9000
```

**PC 2 (port 9001):**
```bash
./hostelD call fe80::PC1_ADDRESS%eth0 9000 9001
```

Or use the GUI: select LAN mode, pick your IPv6 address, set the peer's address and ports.

### 5. Verify

Both peers show a **verification code** (e.g. `7FA5-676E`). Confirm verbally that both match to rule out MITM.

---

## Architecture

### System Overview (~4800 LoC, Rust)

```
src/
├── main.rs             # CLI entry point, command routing
├── voice.rs            # Core engine: handshake, threads, audio, chat, screen
├── crypto.rs           # X25519 key exchange + ChaCha20-Poly1305 encryption
├── screen.rs           # VP8 encode/decode, screen capture, frame chunking
├── audio.rs            # Cross-platform audio (cpal: ALSA/WASAPI/CoreAudio)
├── identity.rs         # Persistent X25519 keypair, fingerprints, contacts
├── chat.rs             # Encrypted chat history (per-contact .enc files)
├── firewall.rs         # Per-IP rate limiting + auto-blacklist
├── sysaudio.rs         # System audio loopback capture (WASAPI/PipeWire)
├── sysfirewall.rs      # OS firewall rule management
├── logger.rs           # File-based logging
├── ui.rs               # Terminal UI (crossterm)
└── gui/
    ├── mod.rs          # App state machine, main update loop
    ├── call.rs         # In-call UI: controls, video, chat, screen share popup
    ├── contacts.rs     # Contact list view
    ├── profile.rs      # Identity/profile display
    ├── settings.rs     # Settings panel
    ├── sidebar.rs      # Navigation sidebar
    └── error.rs        # Error screen
```

### Communication Flow

```
 Peer A                                              Peer B
 ======                                              ======

 1. HANDSHAKE (plaintext)
 ────────────────────────────────────────────────────────────
    generate ephemeral X25519 keypair
    ──── HELLO (0x01) [32B ephemeral pubkey] ────>
    <─── HELLO (0x01) [32B ephemeral pubkey] ─────
    DH shared secret -> session key (SHA-256)
    verification code = SHA-256(secret || "hostelD-verify") -> XXXX-XXXX

 2. IDENTITY EXCHANGE (encrypted)
 ────────────────────────────────────────────────────────────
    ──── IDENTITY (0x03) [32B identity pubkey + nickname] ──>
    <─── IDENTITY (0x03) [32B identity pubkey + nickname] ───
    TOFU check: new contact? known key? key change warning?

 3. ACTIVE CALL (encrypted)
 ────────────────────────────────────────────────────────────
    <─── VOICE (0x02) [Opus frame] ──────────────>   (bidirectional, 50 pkt/s)
    <─── CHAT  (0x04) [text message] ────────────>   (on demand)
    <─── SCREEN(0x06) [VP8 chunk] ───────────────>   (30-60 fps when active)

 4. HANGUP
 ────────────────────────────────────────────────────────────
    ──── HANGUP (0x05) [empty] ──────────────────>   (sent 3x, 50ms apart)
```

### Threading Model

```
 ┌─────────────┐     ring buffer     ┌─────────────────┐
 │ Mic callback │ ─────────────────> │  Sender thread   │
 │  (cpal)      │  f32 samples       │  RNNoise denoise │
 └─────────────┘                     │  + Opus encode   │
                                     │  + encrypt       │ ──── UDP ────>
 ┌─────────────┐     ring buffer     │  + chat send     │
 │ Sys audio   │ ─────────────────> │  (mixed into mic) │
 │ (loopback)  │  f32 samples       └─────────────────┘
 └─────────────┘
                                     ┌─────────────────┐
 ┌─────────────┐     ring buffer     │ Receiver thread  │
 │ Spk callback │ <───────────────── │  UDP recv        │ <─── UDP ─────
 │  (cpal)      │  f32 samples       │  + firewall      │
 └─────────────┘                     │  + decrypt       │
                                     │  + Opus decode   │
 ┌─────────────┐                     │  + chat recv     │
 │ Screen       │ <───────────────── │  + screen chunks │
 │ viewer (GUI) │  RGBA frames       └─────────────────┘
 └─────────────┘

 ┌──────────────────┐
 │ Screen capture    │  (separate thread when sharing)
 │  scrap -> scale   │
 │  -> VP8 encode    │
 │  -> chunk + send  │ ──── UDP (PKT_SCREEN) ────>
 └──────────────────┘
```

### Packet Protocol (UDP)

| Byte | Type | Payload |
|------|------|---------|
| `0x01` | HELLO | 32-byte ephemeral X25519 pubkey (plaintext) |
| `0x02` | VOICE | 4-byte counter + Opus ciphertext + 16B auth tag |
| `0x03` | IDENTITY | Encrypted: 32-byte identity pubkey + nickname |
| `0x04` | CHAT | Encrypted: UTF-8 text message |
| `0x05` | HANGUP | Encrypted: empty payload |
| `0x06` | SCREEN | Encrypted: VP8 chunk with frame assembly header |

Screen chunks header: `[2B frame_id][2B chunk_index][2B total_chunks][1B flags][data...]`

### Audio Pipeline

```
Mic -> [stereo->mono] -> ring buffer -> [RNNoise 2x480] -> [+sys audio mix] -> Opus encode -> encrypt -> UDP
UDP -> decrypt -> Opus decode -> ring buffer -> [mono->stereo] -> Speakers
```

- Opus: 48kHz mono, 960 samples/frame (20ms), 64kbps, VOIP mode
- RNNoise: 480 samples/frame (10ms), 2 frames per Opus frame
- Ring buffers: lock-free SPSC (1 second capacity)
- Stereo devices (e.g. Voicemeeter): auto stereo<->mono conversion at stream boundary

### Screen Sharing Pipeline

```
Display capture (scrap) -> scale to target (no upscale) -> BGRA->I420 -> VP8 encode -> chunk -> encrypt -> UDP
UDP -> decrypt -> reassemble chunks -> VP8 decode -> I420->RGBA -> egui texture
```

- VP8 (libvpx): CBR, realtime, 2 threads, keyframe every 90 frames
- Quality presets: 720p/2Mbps, 1080p/4Mbps, 1080p/6Mbps, 1080p60/8Mbps
- Resolution capped to native (no upscale on small screens)
- Multi-monitor selection
- Chunk pacing: 200us between UDP packets to prevent burst loss

---

## Security

### Encryption

```
 Ephemeral X25519 keypair (per call)
          |
          v
 DH(our_secret, peer_public) -> shared_secret
          |
          ├── SHA-256(secret || "hostelD-voice-key")  -> 256-bit session key
          └── SHA-256(secret || "hostelD-verify")     -> XXXX-XXXX verification code

 Session encryption: ChaCha20-Poly1305
   nonce = [4-byte counter][8 zero bytes]
   packet = [type][counter][ciphertext + 16B tag]
```

### Trust Model (TOFU — Trust On First Use)

1. **First connection** — peer's public key + nickname saved as contact
2. **Subsequent connections** — same key verified, `call_count` incremented
3. **Key change** — WARNING: "different key than previously known! Possible impersonation."
4. **Same address, unknown key** — WARNING: possible impersonation from known contact's address
5. **Nickname change** — informational note (same key, normal behavior)

Trust indicators:
| call_count | Level |
|-----------|-------|
| 1 | New contact — verify code carefully |
| 2-5 | Building trust |
| 6+ | Trusted contact |

### Threat Mitigation

| Threat | Mitigation |
|--------|-----------|
| Eavesdropping | ChaCha20-Poly1305 E2E encryption |
| Man-in-the-middle | Verification code (compare verbally) |
| Replay attacks | Per-packet counter nonce |
| Impersonation | TOFU + key change warnings |
| Packet spam / DoS | Rate limiting (200 pkt/s) + auto-blacklist (5 strikes) |
| Key theft | File permissions (0600 on Unix), local-only storage |
| Identity collision | Full 256-bit pubkey as storage key (not fingerprint) |
| Chat history theft | Encrypted at rest with ChaCha20-Poly1305 (key derived from identity secret) |

### Identity Keys

```
identity.key (64 bytes)
├── [0..32]  X25519 secret key
└── [32..64] X25519 public key

Fingerprint: hD-XXXXXXXX = first 4 bytes of SHA-256(pubkey), hex encoded
  - Display only, never used as storage key
  - Collisions possible at ~65K users (cosmetic, not security-relevant)

Storage key: full 64-char hex of pubkey (collision-proof: 2^256 space)
```

---

## Data Storage

```
~/.hostelD/                                   # Linux/macOS
%USERPROFILE%\.hostelD\                       # Windows
├── identity.key                              # 64 bytes: secret + public key (0600 perms)
├── settings.json                             # nickname, mic, speakers, port, network mode
├── contacts/
│   └── {64-char-pubkey-hex}.json             # one file per known peer
└── chats/
    └── {contact_id}.enc                      # ChaCha20-Poly1305 encrypted chat history
```

Contact IDs are deterministic: `SHA-256(sorted(pubkey_A, pubkey_B))` — both peers derive the same ID.

Chat encryption at rest: `SHA-256(identity_secret || "hostelD-local-storage")` -> key, each file = `[12B nonce][ciphertext + 16B tag]`.

---

## GUI State Machine

```
Setup ──> Connecting ──> KeyWarning ──> InCall ──> Setup
  |            |              |            |
  └────────────┴──────────────┴────────────┴──> Error ──> Setup
```

- **Setup**: device selection, peer address, contacts quick-dial
- **Connecting**: handshake + identity exchange (with cancel)
- **KeyWarning**: TOFU warning, proceed or reject
- **InCall**: voice + chat + screen share controls + video viewer
- **Error**: display error, return to setup

---

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `cargo build` fails "linker cc not found" | Install `build-essential` (Linux), Xcode tools (macOS), or VS Build Tools (Windows) |
| `cmake` not found | `apt install cmake` / `brew install cmake` / install from cmake.org |
| VPX/libvpx not found | Set `VPX_LIB_DIR`, `VPX_INCLUDE_DIR`, `VPX_VERSION` env vars (see setup) |
| No audio devices | Run `./hostelD devices` to check. Ensure audio service is running |
| Handshake timeout | Check firewall rules, verify IPv6 connectivity with `ping6`/`ping` |
| "Address already in use" | Another instance uses that port — change local port |
| GUI won't start (Linux) | Install `libgl1-mesa-dev libegl1-mesa-dev libxkbcommon-dev` |
| fe80:: needs `%interface` | Link-local addresses require zone ID: `fe80::1%eth0` |
| Screen sharing permission (macOS) | Grant Screen Recording in System Settings > Privacy & Security |
