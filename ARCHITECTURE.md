# hostelD — Architecture

~23K LoC Rust. 9 core modules + GUI + messaging daemon + group call engine + file transfer system.

## Code Structure

```
src/
├── main.rs                 # CLI entry, command routing
├── voice.rs                # 1:1 call engine: handshake, audio, chat, screen
├── crypto.rs               # X25519 + ChaCha20-Poly1305, packet encrypt/decrypt
├── audio.rs                # cpal audio I/O (ALSA/WASAPI/CoreAudio)
├── identity.rs             # Keypair, fingerprints, contacts, settings
├── chat.rs                 # Encrypted chat history (1:1 + group)
├── firewall.rs             # Rate limiting + auto-blacklist
├── avatar.rs               # Avatar processing, storage, SHA-256
├── group.rs                # Group data model, channels, members
├── theme.rs                # 18-color theme system
├── screen/                 # VP8 encode/decode, capture, webcam
├── messaging/
│   ├── daemon.rs           # Background daemon: socket, peers, state
│   ├── packets.rs          # Packet dispatch (50+ packet types)
│   ├── protocol.rs         # Send/receive helpers for each packet type
│   ├── commands.rs         # GUI → daemon commands
│   ├── housekeep.rs        # Keepalives, timeouts, retries, avatar ticking
│   └── session.rs          # PeerSession: encrypt/decrypt wrapper
├── filetransfer/
│   ├── sender.rs           # Threaded sender with AIMD congestion control
│   ├── receiver.rs         # Threaded receiver with writer thread
│   └── protocol.rs         # File transfer packet helpers
├── groupcall/
│   ├── engine.rs           # Audio pipeline, mixer, shared types
│   ├── relay.rs            # Leader/member relay mode
│   └── p2p.rs              # P2P mesh mode
└── gui/
    ├── mod.rs              # App state, event loop, tab dispatch
    ├── sidebar.rs          # Navigation tabs + badges
    ├── profile.rs          # Avatar, nickname, fingerprint
    ├── messages.rs         # 1:1 chat + add friend panel
    ├── settings.rs         # Config + blocked/banned management
    ├── call.rs             # 1:1 call UI + screen share popups
    ├── logs.rs             # Log viewer with filter tabs
    └── groups/             # Group UI (sidebar, detail, voice, settings, create)
```

## Packet Protocol (UDP)

All packets except HELLO are encrypted with ChaCha20-Poly1305.

### Voice call (0x01–0x09)

| Byte | Name | Payload |
|------|------|---------|
| `0x01` | HELLO | 32B ephemeral pubkey |
| `0x02` | VOICE | 4B counter + Opus ciphertext + 16B tag |
| `0x03` | IDENTITY | 32B identity pubkey + nickname |
| `0x04` | CHAT | Text message |
| `0x05` | HANGUP | Empty (sent 3x) |
| `0x06` | SCREEN | VP8 chunk |
| `0x07` | SCREEN_STOP | Screen share ended |
| `0x08` | SCREEN_OFFER | Screen share beacon |
| `0x09` | SCREEN_JOIN | Viewer join/leave |

### Messaging daemon (0x10–0x27)

| Byte | Name | Payload |
|------|------|---------|
| `0x10` | MSG_HELLO | 32B ephemeral pubkey |
| `0x11` | MSG_IDENTITY | 32B pubkey + nickname |
| `0x12` | MSG_CHAT | 4B seq + text |
| `0x13` | MSG_ACK | 4B seq (also keepalive when seq=0) |
| `0x14` | MSG_BYE | Disconnect |
| `0x15` | MSG_REQUEST | Contact request |
| `0x16` | MSG_REQUEST_ACK | Request accepted |
| `0x17` | IP_ANNOUNCE | IP + port + timestamp |
| `0x18` | PEER_QUERY | 32B target pubkey |
| `0x19` | PEER_RESPONSE | 32B pubkey + IP + port |
| `0x1A` | PRESENCE | 1B status (Online/Away) |
| `0x1B–0x22` | FILE_* | File transfer (offer/accept/reject/chunk/ack/complete/cancel/nack) |
| `0x23` | MSG_CONFIRM | Identity-bound key upgrade |
| `0x24–0x26` | AVATAR_* | Avatar offer/data/ack |
| `0x27` | AVATAR_NACK | Avatar needed (request data) |

### Group call (0x30–0x47)

| Byte | Name | Payload |
|------|------|---------|
| `0x30` | GRP_HELLO | 32B pubkey + 16B group_id |
| `0x31` | GRP_VOICE | 2B sender_idx + 4B counter + ciphertext |
| `0x32` | GRP_CHAT | 2B sender_idx + 4B counter + text |
| `0x33` | GRP_HANGUP | Member leaving (sent 3x) |
| `0x34` | GRP_ROSTER | JSON roster (leader → members) |
| `0x35–0x36` | GRP_PING/PONG | RTT measurement |
| `0x37` | GRP_LEADER | New leader announcement |
| `0x38` | GRP_INVITE | Group invite via daemon |
| `0x3C` | GRP_MSG_CHAT | Offline group chat |
| `0x3F` | GRP_UPDATE | Group metadata update |
| `0x40–0x46` | GRP_AVATAR_* | Group avatar protocol |
| `0x47` | GRP_CALL_SIGNAL | Voice channel presence |

## Threading Model

### 1:1 Call (voice.rs)

```
Mic callback (cpal) → ring buffer → Sender thread (denoise + opus + encrypt + UDP)
                                     Receiver thread (UDP → decrypt → opus → ring buffer) → Speaker callback
                                     Screen capture thread (scrap → VP8 → encrypt → UDP)
```

### Group Call (relay.rs / p2p.rs)

- **P2P**: every peer sends to every peer (mesh)
- **Relay**: members send to leader, leader relays to all others
- Both spawn: sender thread + receiver thread + mixer thread + housekeeping thread

### Messaging Daemon (daemon.rs)

Single-threaded loop: `process_commands() → receive_packets() → drain_background() → housekeep()`

Background threads spawned per-transfer:
- SHA-256 hashing thread (pre-send)
- Sender thread with AIMD congestion control
- Receiver writer thread
- Post-receive verification thread

## Key Exchange

```
Ephemeral X25519 keypair (per session)
    DH(secret, peer_public) → shared_secret
    SHA-256(secret || "hostelD-voice-key") → session key
    SHA-256(secret || "hostelD-verify")    → XXXX-XXXX verification code

Known contacts: upgrade with identity DH
    SHA-256(ephemeral_DH || identity_DH || "hostelD-msg-key") → upgraded key
    Both send CONFIRM encrypted with upgraded key to prove identity
```

## Data Storage

```
~/.hostelD/
├── identity.key                    # 64B: 32 secret + 32 public (0600 perms)
├── settings.json                   # Nickname, devices, port, theme, bans
├── contacts/{pubkey_hex}.json      # One file per contact
├── chats/{contact_id}.enc          # Encrypted 1:1 chat history
├── chats/grp_{group_id}_{ch}.enc   # Encrypted group chat history
├── groups/{group_id}.json          # Group metadata + members
├── avatars/own.png                 # Own avatar
├── avatars/{contact_id}.png        # Contact avatars
├── avatars/group_{group_id}.png    # Group avatars
├── files/{contact_id}/             # Received files
└── files_tmp/                      # In-progress transfers
```

## File Transfer

ACK-on-Error protocol with AIMD congestion control:

1. Sender hashes file (background thread) → sends FILE_OFFER
2. Receiver accepts → FILE_ACCEPT
3. Sender thread blasts chunks (4KB each), pacing with adaptive window:
   - Start: 10 chunks/batch
   - No loss → double (slow start)
   - <5% loss → +10 (additive increase)
   - ≥5% loss → halve (multiplicative decrease)
4. Sender sends FILE_COMPLETE → receiver checks for gaps
5. Missing chunks → NACK → sender retransmits only those → repeat
6. All chunks received → SHA-256 verify → FILE_ACK → done

## Platform Dependencies

| Component | Windows | macOS | Linux |
|-----------|---------|-------|-------|
| Audio | WASAPI | CoreAudio | ALSA/PipeWire |
| Webcam | MediaFoundation | AVFoundation | V4L2 |
| Screen capture | DXGI | CGDisplayStream | X11/PipeWire |
| System audio | WASAPI loopback | ScreenCaptureKit | PipeWire Monitor |
| Firewall | netsh | App prompt | ufw |
| Notifications | PowerShell | osascript | notify-send |

`Cargo.toml` is per-platform (`.gitignore`d). Copy the right variant before building:
- `copy Cargo.toml.windows Cargo.toml`
- `cp Cargo.toml.macos Cargo.toml`
- `cp Cargo.toml.linux Cargo.toml`
