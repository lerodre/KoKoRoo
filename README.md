# hostelD

> **Alpha software.** Built with [Claude Code](https://claude.ai/code). Functional but under active development. Expect bugs and breaking changes.

P2P encrypted voice, chat, groups, file transfer, and screen sharing over IPv6 UDP. No servers, no accounts, no registration.

```
 ┌──────────┐       UDP/IPv6        ┌──────────┐
 │  Peer A  │ <───────────────────> │  Peer B  │
 │          │   E2E Encrypted       │          │
 │ identity │   Voice + Chat +      │ identity │
 │  .key    │   Files + Screen      │  .key    │
 └──────────┘                       └──────────┘
```

## Networking

hostelD requires IPv6 between peers. If your ISP doesn't provide IPv6 or you want a private network, use an overlay like [ZeroTier](https://zerotier.com) or [Tailscale](https://tailscale.com). Both provide IPv6 addresses that work with hostelD out of the box and give you control over who can reach your node.

Without an overlay, anyone with your IPv6 address and port can attempt to connect. **Be careful who you add as a contact.**

## Features

- **Voice calls** - Opus 64kbps, RNNoise suppression, full duplex
- **Group calls** - P2P mesh or relay mode, multi-peer voice + chat
- **E2E encryption** - X25519 + ChaCha20-Poly1305 (same primitives as Signal/WireGuard)
- **Messaging** - 1:1 and group text chat, encrypted in transit and at rest
- **File transfer** - Drag & drop, chunked UDP with AIMD congestion control
- **Screen & webcam sharing** - VP8 encoded, multi-monitor, system audio capture
- **Avatars** - Profile and group avatars with offer-wait protocol
- **Contact system** - Friend requests, TOFU trust, presence (online/away/offline)
- **Anti-spam** - Per-IP rate limiting, auto-blacklist, OS firewall integration
- **Themes** - 18 customizable colors with smart randomize
- **Cross-platform** - Windows, macOS, Linux (GUI + TUI)

## Quick Start

### Windows

```powershell
# Prerequisites: Visual Studio Build Tools ("Desktop dev with C++"), CMake, Rust
git clone https://github.com/lerodre/hostelD.git && cd hostelD
copy Cargo.toml.windows Cargo.toml
cargo build --release
.\target\release\hostelD.exe
```

For screen sharing, install libvpx via vcpkg and set `VPX_LIB_DIR`, `VPX_INCLUDE_DIR`, `VPX_VERSION` env vars. See [ARCHITECTURE.md](ARCHITECTURE.md) for details.

### macOS

```bash
# Prerequisites: Xcode CLI tools, Homebrew
brew install cmake opus libvpx pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
git clone https://github.com/lerodre/hostelD.git && cd hostelD
cp Cargo.toml.macos Cargo.toml
cargo build --release
./target/release/hostelD
```

### Linux

```bash
sudo apt install build-essential pkg-config cmake libasound2-dev libopus-dev \
    libvpx-dev libxkbcommon-dev libgtk-3-dev libgl1-mesa-dev libegl1-mesa-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
git clone https://github.com/lerodre/hostelD.git && cd hostelD
cp Cargo.toml.linux Cargo.toml
cargo build --release
./target/release/hostelD
```

> **Note:** `Cargo.toml` is per-platform (`.gitignore`d). Always copy the right variant before building.

## CLI

```
hostelD              # GUI (default)
hostelD tui          # Terminal UI
hostelD call <ip> <port> <local-port>
hostelD devices      # List audio devices
hostelD mic-test     # Audio loopback test
```

## Security

| What | How |
|------|-----|
| Key exchange | Ephemeral X25519 per session |
| Encryption | ChaCha20-Poly1305 (all packets) |
| Replay protection | Per-packet counter nonce |
| Trust model | TOFU + key change warnings |
| MITM detection | Verification code (compare verbally) |
| Chat storage | Encrypted at rest (ChaCha20-Poly1305) |
| Anti-spam | Rate limiting (1000 pkt/s) + auto-blacklist |

## Documentation

- **[ARCHITECTURE.md](ARCHITECTURE.md)** - Protocol, threading, packet types, data layout, code structure
- **[FEATURES.md](FEATURES.md)** - Complete feature list with details
- **[KNOWN_ISSUES.md](KNOWN_ISSUES.md)** - Known bugs, platform issues, and security weaknesses

