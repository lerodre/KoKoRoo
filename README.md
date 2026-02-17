# hostelD

P2P encrypted voice calls + chat over IPv6, built in Rust.

```
Peer A  ◄──── E2E Encrypted (ChaCha20-Poly1305) ────►  Peer B
  Voice (Opus 64kbps) + Chat + Identity
  over IPv6 UDP, no central server
```

## Features

- **Voice calls** — Opus codec at 64kbps, full duplex
- **E2E encryption** — X25519 key exchange + ChaCha20-Poly1305 (same as Signal/WireGuard)
- **Real-time chat** — encrypted in transit and at rest
- **Persistent identity** — `hD-XXXXXXXX` fingerprints, contact management
- **Anti-spam firewall** — rate limiting + auto-blacklist after 5 strikes
- **Desktop GUI** (eframe/egui) and **Terminal UI** (crossterm)
- **LAN & Internet modes** — filters IPv6 addresses by scope

---

## Setup — Linux (Ubuntu/Debian)

### 1. Install system dependencies

```bash
sudo apt update
sudo apt install -y build-essential pkg-config cmake \
    libasound2-dev libopus-dev \
    libxkbcommon-dev libgtk-3-dev \
    libgl1-mesa-dev libegl1-mesa-dev
```

What each package is for:
| Package | Why |
|---------|-----|
| `build-essential` | C compiler (gcc), needed by some Rust crates |
| `pkg-config` | Finds system libraries at build time |
| `cmake` | Builds libopus from source (audiopus_sys) |
| `libasound2-dev` | ALSA headers — audio backend for `cpal` |
| `libopus-dev` | Opus codec library |
| `libxkbcommon-dev` | Keyboard input for GUI (winit/egui) |
| `libgtk-3-dev` | Native file dialogs (egui) |
| `libgl1-mesa-dev` | OpenGL headers for GUI rendering (glow) |
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

The binary will be at `./target/release/hostelD` (~16MB).

### 4. Run

```bash
# Launch the GUI (default)
./target/release/hostelD

# Launch the terminal UI
./target/release/hostelD tui

# Direct CLI call
./target/release/hostelD call <peer-ipv6> <peer-port> <local-port>
```

### PipeWire / PulseAudio note

On modern Ubuntu (22.04+), audio runs through PipeWire with ALSA compatibility. `cpal` uses ALSA by default, which works transparently through PipeWire. No extra config needed.

---

## Setup — Windows

### 1. Install Rust

Download and run the installer from [rustup.rs](https://rustup.rs). Accept the defaults (MSVC toolchain).

### 2. Install Visual Studio Build Tools

Download from [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/).

In the installer, select **"Desktop development with C++"**. This installs:
- MSVC compiler (needed by some Rust crates)
- Windows SDK (needed for audio/GUI)

### 3. Install CMake

Download from [cmake.org](https://cmake.org/download/). During install, select **"Add CMake to the system PATH"**.

### 4. Clone and build

Open a terminal (PowerShell or cmd):

```powershell
git clone https://github.com/lerodre/hostelD.git
cd hostelD
cargo build --release
```

The binary will be at `.\target\release\hostelD.exe`.

### 5. Run

```powershell
# Launch the GUI
.\target\release\hostelD.exe

# Direct CLI call
.\target\release\hostelD.exe call <peer-ipv6> <peer-port> <local-port>
```

### Windows audio note

`cpal` uses WASAPI on Windows (built-in, no extra drivers needed).

---

## Testing on LAN (2 PCs)

### 1. Find your IPv6 addresses

**Linux:**
```bash
ip -6 addr show | grep "scope link"
# Look for fe80::xxxx addresses
```

**Windows:**
```powershell
ipconfig
# Look for "Link-local IPv6 Address" under your network adapter
# Example: fe80::1a2b:3c4d:5e6f:7890%eth0
```

### 2. Make sure both PCs can reach each other

```bash
# From Linux, ping Windows (use the Windows fe80:: address)
ping6 fe80::WINDOWS_ADDRESS%eth0

# From Windows, ping Linux
ping fe80::LINUX_ADDRESS%eth0
```

Replace `eth0` with your actual network interface name (`enp3s0`, `wlp2s0`, `Ethernet`, `Wi-Fi`, etc.).

### 3. Allow through Windows Firewall

On the Windows PC, allow the app through the firewall:

```powershell
# Run as Administrator
netsh advfirewall firewall add rule name="hostelD" dir=in action=allow protocol=UDP localport=9000-9100
```

Or: Windows will prompt you to allow network access the first time you run the app — click **"Allow access"**.

### 4. Start the call

**PC 1 (Linux, port 9000):**
```bash
./target/release/hostelD call fe80::WINDOWS_ADDRESS%eth0 9001 9000
```

**PC 2 (Windows, port 9001):**
```powershell
.\target\release\hostelD.exe call fe80::LINUX_ADDRESS%eth0 9000 9001
```

Or use the GUI on both — select **LAN mode**, pick your IPv6 address, and set the peer's address and ports.

### 5. Verify the connection

Both peers will show:
- A **verification code** (e.g. `7FA5-676E`) — confirm verbally that both match
- **Peer identity** (e.g. `hD-BD9399A1`)
- "Voice call active! (encrypted)"

### 6. Test chat

In the GUI, type a message in the chat box at the bottom and press Enter or click Send. Messages are encrypted and saved locally in `~/.hostelD/chats/` (Linux) or `%USERPROFILE%\.hostelD\chats\` (Windows).

---

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `cargo build` fails with "linker cc not found" | Install `build-essential` (Linux) or Visual Studio Build Tools (Windows) |
| `cmake` not found | `sudo apt install cmake` (Linux) or install from cmake.org (Windows) |
| No audio devices found | Check `./hostelD devices` — make sure ALSA/PipeWire is running (Linux) or audio service is on (Windows) |
| Handshake timeout (30s) | Check firewall rules, verify IPv6 connectivity with `ping6`/`ping` |
| "Address already in use" | Another instance is using that port — change the local port |
| GUI won't start (Linux) | Install `libgl1-mesa-dev libegl1-mesa-dev libxkbcommon-dev` and rebuild |
| fe80:: address needs `%interface` | Link-local addresses require a zone ID suffix: `fe80::1%eth0` |

---

## Project Architecture

See [PHASES.md](PHASES.md) for detailed phase-by-phase development documentation.

```
src/
├── main.rs         # CLI entry point and command routing
├── audio.rs        # Audio capture/playback (cpal)
├── chat.rs         # Encrypted chat history storage
├── crypto.rs       # E2E encryption (X25519 + ChaCha20-Poly1305)
├── firewall.rs     # Anti-spam IP blacklist and rate limiting
├── gui.rs          # Desktop GUI (eframe/egui)
├── identity.rs     # Persistent identity and contact management
├── voice.rs        # Voice + chat engine (Opus + encryption + UDP)
└── ui.rs           # Terminal UI (crossterm)
```

## Local Data

```
~/.hostelD/
├── identity.key              # Your X25519 keypair (permissions 0600)
├── contacts/
│   └── hD-XXXXXXXX.json     # Saved contacts
└── chats/
    └── <contact_id>.enc     # Encrypted chat history
```
