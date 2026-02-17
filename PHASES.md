# hostelD — Development Phases

P2P voice communication over IPv6, built in Rust.

```
┌──────────┐       IPv6 / UDP        ┌──────────┐
│  Peer A  │◄───────────────────────►│  Peer B  │
│          │                          │          │
│  Mic ──► Encode ──► Send           │          │
│  Spk ◄── Decode ◄── Recv          │          │
└──────────┘                          └──────────┘
```

---

## Phase 1: UDP Echo ✅

**Goal:** Prove IPv6 UDP networking works.

**What it does:**
- Binds a UDP socket to `[::]:port` (all IPv6 interfaces)
- Sends/receives text messages between two peers
- Echoes messages back to the sender

**Files:** `src/main.rs` (`listen` and `send` functions)

**Usage:**
```bash
# Terminal 1: listen for messages
cargo run -- listen 9000

# Terminal 2: send a message
cargo run -- send ::1 9000 "hello!"
```

**Key concepts:**
- `UdpSocket::bind("[::]:9000")` — binds to IPv6
- `::1` — IPv6 loopback (localhost)
- `recv_from()` / `send_to()` — stateless packet exchange
- UDP = no connection, no retransmission, low latency

---

## Phase 2: Audio Capture & Playback ✅

**Goal:** Capture mic input and play audio through speakers.

**What it does:**
- Lists all audio input/output devices on the system
- Captures mic audio as f32 samples (mono, 48kHz)
- Plays audio through speakers via a lock-free ring buffer
- Loopback test: hear your own mic in real time

**Files:** `src/audio.rs`

**Usage:**
```bash
# List all audio devices
cargo run -- devices

# Loopback test (use headphones to avoid feedback!)
cargo run -- mic-test
```

**Architecture:**
```
┌───────┐    ring buffer    ┌──────────┐
│  Mic  │ ──► [f32 x 48k] ──►│ Speakers │
│ (cpal │    (lock-free)    │  (cpal   │
│  input)│                   │  output) │
└───────┘                    └──────────┘
```

**Key concepts:**
- `cpal` — cross-platform audio (ALSA/PipeWire on Linux, CoreAudio on macOS, WASAPI on Windows)
- Ring buffer (`ringbuf`) — lock-free SPSC queue between mic and speaker threads
- 48kHz mono f32 — standard voice quality

---

## Phase 3: Voice over UDP ✅

**Goal:** Send live voice between two peers over IPv6.

**What it does:**
- Captures mic → packs into UDP packets → sends to peer
- Receives UDP packets from peer → plays through speakers
- Full duplex: both peers talk and listen simultaneously
- 480 samples per packet = 10ms of audio = 1920 bytes

**Files:** `src/voice.rs`

**Usage:**
```bash
# Peer A (port 9000, talks to peer B on port 9001)
cargo run -- call ::1 9001 9000

# Peer B (port 9001, talks to peer A on port 9000)
cargo run -- call ::1 9000 9001
```

**Architecture:**
```
Peer A:
┌───────┐    mic_rb     ┌────────┐   UDP    ┌─────────┐
│  Mic  │ ──► [f32] ──► │ Sender │ ───────► │  Peer B │
└───────┘               │ Thread │          │ port    │
                         └────────┘          └─────────┘

┌─────────┐   UDP    ┌──────────┐   spk_rb    ┌──────────┐
│  Peer B │ ───────► │ Receiver │ ──► [f32] ──►│ Speakers │
│ port    │          │ Thread   │              └──────────┘
└─────────┘          └──────────┘
```

**4 threads per peer:**
1. Mic callback (cpal) → pushes f32 to `mic_rb`
2. Sender thread → pops from `mic_rb`, packs bytes, sends UDP
3. Receiver thread → receives UDP, unpacks f32, pushes to `spk_rb`
4. Speaker callback (cpal) → pops from `spk_rb`, plays audio

**Packet format (raw PCM, no compression):**
- 480 f32 samples × 4 bytes = 1920 bytes per packet
- 48000 Hz / 480 = 100 packets/sec
- Bandwidth: ~192 KB/s (~1.5 Mbps) per direction

---

## Phase 4: Opus Compression ✅

**Goal:** Compress voice to reduce bandwidth from ~1.5 Mbps to ~64 kbps.

**What it does:**
- Encodes mic audio with Opus codec before sending over UDP
- Decodes received Opus packets before playing through speakers
- 960 samples per frame @ 48kHz = 20ms per frame (standard Opus frame size)
- Bitrate set to 64 kbps — clear voice quality

**Files:** `src/voice.rs` (shared engine used by both CLI and UI)

**Dependencies:** `audiopus` 0.3 (Rust bindings for libopus)

**Bandwidth comparison:**

| Mode | Packet size | Packets/sec | Bandwidth |
|------|-------------|-------------|-----------|
| Raw PCM (Phase 3) | 1920 bytes | 100/sec | ~1.5 Mbps |
| Opus 64kbps (Phase 4) | ~80-160 bytes | 50/sec | ~64 kbps |

**~24x bandwidth reduction!**

**Architecture:**
```
Mic → [ring buf] → Opus Encoder → UDP send (~80-160 bytes/packet)
                                      ↓ network
Spk ← [ring buf] ← Opus Decoder ← UDP recv
```

---

## Phase 5: Security ✅

**Goal:** E2E encryption + anti-spam firewall.

### E2E Encryption (anti-MITM)

**Protocol:**
```
Peer A                              Peer B
  │── HELLO [X25519 pubkey] ──────►│
  │◄── HELLO [X25519 pubkey] ──────│
  │                                 │
  │  Both derive shared secret      │
  │  via X25519 Diffie-Hellman      │
  │                                 │
  │  Verification code: DA0C-FED4   │  ← compare verbally!
  │                                 │
  │── VOICE [nonce][ChaCha20] ────►│
  │◄── VOICE [nonce][ChaCha20] ────│
```

**Crypto stack:**
- **Key exchange:** X25519 (same as Signal, WireGuard)
- **Encryption:** ChaCha20-Poly1305 AEAD (same as TLS 1.3, WireGuard)
- **Verification:** SHA-256 derived 8-char code (XXXX-XXXX)

**Packet format:**
```
HELLO: [0x01][32-byte X25519 public key]
VOICE: [0x02][4-byte nonce counter][ciphertext + 16-byte auth tag]
```

**Files:** `src/crypto.rs`

### Anti-Spam Firewall

**Rules:**
- Rate limit: >200 packets/sec from one IP → strike
- Auth failure: decryption fails → strike
- 5 strikes → IP blacklisted (silently dropped)
- Blacklisted IPs get no response (attacker can't tell if port is open)

**Files:** `src/firewall.rs`

---

## Phase 6: Interactive UI ✅

**Goal:** Simple terminal interface with LAN/Internet mode selection.

**What it does:**
- Select network mode: LAN (link-local) or Internet (global IPv6)
- Arrow-key menu to select audio output/input devices
- Filters IPv6 addresses by mode (link-local for LAN, global for Internet)
- Internet mode shows firewall warning
- Live call screen with verification code, SPACE to mute, Q to quit

**Files:** `src/ui.rs`

**Usage:**
```bash
cargo run -- start
```

**UI flow:**
```
┌──────────────────────────────┐
│ 1. Select mode: LAN/Internet │ ◄── arrow keys
│ 2. Select IPv6 address       │ ◄── filtered by mode
│ 3. Select speakers           │
│ 4. Select microphone         │
│ 5. Enter port + peer         │ ◄── type + enter
│ 6. Key exchange handshake    │ ◄── automatic
│ 7. Secure voice call         │ ◄── SPACE=mute, Q=quit
└──────────────────────────────┘
```

**Call screen shows:**
```
╔════════════════════════════════════════════╗
║        hostelD — Secure Voice Call         ║
║  Peer:   [::1]:9000                       ║
║  Mode:   LAN                              ║
║  Codec:  Opus 64kbps                      ║
║  Status: ENCRYPTED                        ║
║  Verify: DA0C-FED4                        ║
║  ^ Ask your peer for their code.          ║
║      Microphone: [|||]  ON                ║
║  SPACE = toggle mic  |  Q = hang up       ║
╚════════════════════════════════════════════╝
```

---

## Phase 7: GUI Application ✅

**Goal:** Visual desktop GUI (not console) — flat and simple.

**What it does:**
- Cross-platform desktop window using `eframe`/`egui`
- Setup screen: network mode, IPv6 address, mic/speaker selection, port config
- Connecting screen: spinner while handshake runs
- In-call screen: status bar, verification code, mic toggle, hang up button
- Real-time encrypted chat panel with scrollable history and text input
- Launches by default (no args); `tui` command for terminal UI

**Files:** `src/gui.rs`

**Usage:**
```bash
# Launch GUI (default)
cargo run --release

# Or explicitly
cargo run --release -- gui

# Terminal UI still available
cargo run --release -- tui
```

**GUI flow:**
```
┌──────────────────────────────────┐
│  Setup Screen                    │
│  ┌─ Network Mode: [LAN ▼] ────┐ │
│  │  IPv6 Address: [fe80::1 ▼]  │ │
│  │  Microphone:   [default ▼]  │ │
│  │  Speakers:     [default ▼]  │ │
│  │  Local Port:   [9000     ]  │ │
│  │  Peer Address: [::1      ]  │ │
│  │  Peer Port:    [9001     ]  │ │
│  └──────────────────────────────┘ │
│          [ 📞 Call ]              │
├──────────────────────────────────┤
│  In-Call Screen                  │
│  🔒 Encrypted | Peer: hD-XXXX   │
│  Verify: 7FA5-676E | Opus 64k   │
│  [🎤 Mute] [📞 Hang Up]         │
│  ┌─ Chat ───────────────────┐    │
│  │ 14:32 them: hello!       │    │
│  │ 14:32 me: hey there      │    │
│  └──────────────────────────┘    │
│  [type message...     ] [Send]   │
└──────────────────────────────────┘
```

**Window:** 450×650, min 400×550

---

## Phase 8: Identity & Encrypted Chat ✅

**Goal:** Persistent user identity, contact management, and E2E encrypted real-time chat with local storage.

### Identity System

**How it works:**
- Each user gets a persistent X25519 keypair stored at `~/.hostelD/identity.key`
- Keypair is generated on first run and reused forever
- Fingerprint format: `hD-XXXXXXXX` (SHA-256 of public key, truncated)
- Identity is exchanged after the ephemeral key handshake (double encryption layer)

**Files:** `src/identity.rs`

**Protocol:**
```
Peer A                                    Peer B
  │── HELLO [ephemeral pubkey] ─────────►│  (Phase 5 handshake)
  │◄── HELLO [ephemeral pubkey] ─────────│
  │  derive shared secret, session key    │
  │                                       │
  │── IDENTITY [encrypted identity key] ─►│  (new: Phase 8)
  │◄── IDENTITY [encrypted identity key] ─│
  │                                       │
  │  Both derive contact_id from sorted   │
  │  identity pubkeys (same on both)      │
  │                                       │
  │── VOICE [encrypted opus] ───────────►│
  │── CHAT  [encrypted text] ───────────►│
```

### Contact Management

**How it works:**
- On first connection with a peer, a contact is created
- Contact ID: SHA-256(sorted(pubkey_A, pubkey_B)) → 16-char hex — same on both sides
- Contacts stored as JSON at `~/.hostelD/contacts/{fingerprint}.json`
- Tracks: fingerprint, public key, nickname, contact_id, first_seen, last_seen

### Encrypted Chat

**How it works:**
- Chat messages sent as `PKT_CHAT` (0x04) packets, encrypted with the session key
- Both peers store chat history locally, encrypted at rest
- Storage key: SHA-256(identity_secret || "hostelD-storage") → ChaCha20-Poly1305
- Chat files: `~/.hostelD/chats/{contact_id}.enc`
- History loads automatically when reconnecting with a known contact

**Files:** `src/chat.rs`

**Packet types (complete protocol):**
```
0x01  HELLO     [32-byte X25519 ephemeral pubkey]
0x02  VOICE     [4-byte nonce][Opus ciphertext + 16-byte tag]
0x03  IDENTITY  [4-byte nonce][identity pubkey ciphertext + 16-byte tag]
0x04  CHAT      [4-byte nonce][UTF-8 text ciphertext + 16-byte tag]
```

**Local storage structure:**
```
~/.hostelD/
├── identity.key                    # 64 bytes (32 secret + 32 public), mode 0600
├── contacts/
│   └── hD-XXXXXXXX.json           # Per-contact metadata
└── chats/
    └── f3c22945910f73c0.enc       # Encrypted chat history (JSON + ChaCha20)
```

---

## Phase 9: NAT Traversal & Discovery (future)

**Goal:** Connect peers across the internet without manual IP exchange.

**Ideas:**
- mDNS for LAN peer discovery
- STUN for NAT hole punching
- Simple relay/signaling server as fallback

---

## Project Structure

```
hostelD/
├── Cargo.toml          # Dependencies and project config
├── PHASES.md           # This file
└── src/
    ├── main.rs         # CLI entry point and commands
    ├── audio.rs        # Audio capture and playback (cpal)
    ├── chat.rs         # Encrypted chat history storage
    ├── crypto.rs       # E2E encryption (X25519 + ChaCha20-Poly1305)
    ├── firewall.rs     # Anti-spam IP blacklist and rate limiting
    ├── gui.rs          # Desktop GUI application (eframe/egui)
    ├── identity.rs     # Persistent identity and contact management
    ├── voice.rs        # Voice + chat engine (Opus + encryption + UDP)
    └── ui.rs           # Interactive terminal UI
```

## How to Build & Run

```bash
# Install dependencies (Linux)
sudo apt install libasound2-dev libopus-dev pkg-config

# Build
cargo build --release

# Launch GUI (default)
./target/release/hostelD

# Launch terminal UI
./target/release/hostelD tui

# Direct call (E2E encrypted)
./target/release/hostelD call <peer-ipv6> <peer-port> <local-port>

# Utility commands
./target/release/hostelD devices     # List audio devices
./target/release/hostelD mic-test    # Mic loopback test
```
