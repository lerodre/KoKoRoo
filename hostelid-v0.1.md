# hostelD v0.1 — System Documentation

## Overview

hostelD is a peer-to-peer encrypted voice + chat application. No central server, no accounts, no registration. Identity is a local X25519 keypair stored on disk. Communication is encrypted end-to-end using ChaCha20-Poly1305.

---

## Architecture

```
 ┌──────────┐     UDP/IPv6      ┌──────────┐
 │  Peer A  │ <────────────────> │  Peer B  │
 │          │   E2E Encrypted    │          │
 │ identity │   Voice + Chat     │ identity │
 │  .key    │                    │  .key    │
 └──────────┘                    └──────────┘
```

### No Server Required

- Direct peer-to-peer over IPv6 (UDP)
- Both peers must know each other's IP and port
- Works on LAN (link-local) or Internet (global IPv6)

---

## Identity Model

### Key Generation

- On first launch, a random 256-bit X25519 keypair is generated
- Stored in `~/.hostelD/identity.key` (64 bytes: 32 secret + 32 public)
- File permissions: `0600` (owner-only)

### Fingerprint (Display Only)

```
hD-XXXXXXXX   (4 bytes = 32 bits of SHA-256)
```

- Short, human-readable identifier
- Used ONLY for visual display, never as a storage key
- Can collide at ~65K users (birthday paradox) — this is acceptable because it's cosmetic

### Public Key (Storage Key)

```
64 hex characters   (full 32-byte public key)
```

- Used as the filename for contact storage: `{pubkey_hex}.json`
- Collision-proof: 2^256 space, requires ~2^128 attempts to collide
- This is the TRUE identifier of a peer

### Nickname

- Optional, user-chosen display name
- Persisted in `~/.hostelD/settings.json`
- Sent during identity exchange: `[32-byte pubkey][utf8 nickname]`
- Display format: `nickname #fingerprint` (e.g., `Alice #hD-A7F3B2E1`)

---

## Trust Model: TOFU (Trust On First Use)

### How It Works

1. **First connection**: You connect to a peer for the first time. Their public key and nickname are saved. Trust is established.
2. **Subsequent connections**: The system recognizes the peer by their public key. Each call increments `call_count`. Trust grows over time.
3. **Key change detected**: If someone connects with a known nickname but a DIFFERENT public key, a WARNING is displayed:

```
WARNING: "Alice" connected with a DIFFERENT key than previously known!
Possible impersonation.
```

4. **Nickname change detected**: If a known public key connects with a new nickname, an informational note is shown (not a warning — this is normal).

### Why This Works

To impersonate someone, an attacker would need to:
- Steal the victim's `identity.key` file (requires file system access)
- OR break X25519 (computationally infeasible)
- AND replicate the entire relationship history (chat history is encrypted and tied to the original keys)

### Trust Indicators

| call_count | Trust Level |
|-----------|-------------|
| 1 | New contact — verify code carefully |
| 2-5 | Building trust |
| 6+ | "Trusted contact" printed in console |

---

## Cryptography

### Key Exchange (Per Call)

1. **Ephemeral X25519**: Each call generates a new ephemeral keypair
2. **HELLO packets** (`0x01`): Exchange ephemeral public keys
3. **Diffie-Hellman**: Derive shared secret
4. **Key derivation**: `SHA-256(shared_secret || "hostelD-voice-key")` → 256-bit symmetric key
5. **Verification code**: `SHA-256(shared_secret || "hostelD-verify")` → `XXXX-XXXX` display code

### Session Encryption

- **Cipher**: ChaCha20-Poly1305 (same as TLS 1.3, WireGuard)
- **Nonce**: 12 bytes, first 4 bytes = packet counter
- **Packet format**: `[type][4-byte counter][ciphertext + 16-byte auth tag]`

### Identity Exchange

After session is established:
- **IDENTITY packets** (`0x03`): Send `[32-byte identity pubkey][nickname bytes]` encrypted
- This proves the peer controls the long-term identity key

### Packet Types

| Byte | Type | Description |
|------|------|-------------|
| `0x01` | HELLO | Key exchange (plaintext ephemeral pubkey) |
| `0x02` | VOICE | Encrypted Opus audio frame |
| `0x03` | IDENTITY | Encrypted identity pubkey + nickname |
| `0x04` | CHAT | Encrypted chat message |
| `0x05` | HANGUP | Encrypted empty (mutual disconnect) |

---

## Audio

- **Codec**: Opus at 64 kbps (VoIP optimized)
- **Sample rate**: 48 kHz mono
- **Frame size**: 960 samples = 20ms
- **Ring buffers**: Lock-free single-producer single-consumer for mic → encoder and decoder → speakers

---

## Data Storage

### Directory Structure

```
~/.hostelD/
  identity.key          # 64 bytes: [32 secret][32 public]
  settings.json         # nickname, mic, speakers, port
  contacts/
    {pubkey_hex}.json   # one file per known peer (64-char hex filename)
  chats/
    {contact_id}.enc    # encrypted chat history per relationship
```

### Contact File Format

```json
{
  "fingerprint": "hD-A7F3B2E1",
  "pubkey": [/* 32 bytes */],
  "nickname": "Alice",
  "contact_id": "a7f3b2e1c4d5e6f7",
  "first_seen": "1708200000",
  "last_seen": "1708300000",
  "last_address": "::1",
  "last_port": "9000",
  "call_count": 5
}
```

### Settings File Format

```json
{
  "nickname": "Bob",
  "mic": "HDA Intel PCH: ALC255",
  "speakers": "HDA Intel PCH: ALC255",
  "local_port": "9000"
}
```

### Chat History

- Encrypted with `SHA-256(identity_secret || "hostelD-local-storage")`
- Each chat file: `[12-byte nonce][ciphertext + 16-byte tag]`
- Decrypted to JSON array of `{from_me, text, timestamp}`

---

## Mutual Hang Up Protocol

1. **Local hang up**: User clicks "Hang Up" → `local_hangup` flag set → `running` set to false
2. **Sender thread exits loop**: Checks `local_hangup` → sends 3x `PKT_HANGUP` with 50ms gaps
3. **Remote peer**: Receiver thread decrypts `PKT_HANGUP` → sets `running` to false → call ends on both sides
4. **GUI detection**: `update()` loop checks `running` → if false while `InCall`, triggers cleanup

---

## Firewall

- Per-IP rate limiting: 200 packets/second max
- Decrypt failure tracking: 5 strikes → IP blacklisted
- Protects against basic DoS and packet spam during a call

---

## GUI Features

- **Setup screen**: Nickname input, network mode (LAN/Internet), IPv6 selector with Copy button, audio device selection, port config, contact list
- **Contact list**: Shows all known contacts sorted by last seen, click to auto-fill peer IP/port
- **In-call screen**: TOFU warning banner (if applicable), verification code, mic toggle, hang up button, chat with history
- **Settings persistence**: Mic, speakers, port, nickname saved across sessions

---

## Interfaces

| Interface | Command | Description |
|-----------|---------|-------------|
| GUI | `hostelD` or `hostelD gui` | Full graphical interface |
| TUI | `hostelD tui` | Terminal interactive menus |
| CLI | `hostelD call <ip> <port> <local_port>` | Direct voice call |

---

## Security Summary

| Threat | Mitigation |
|--------|-----------|
| Eavesdropping | ChaCha20-Poly1305 E2E encryption |
| MITM | Verification code (compare verbally) |
| Replay attacks | Per-packet counter nonce |
| Impersonation | TOFU + key change warnings + call count |
| Packet spam | Rate limiting + auto-blacklist firewall |
| Key theft | File permissions (0600), local-only storage |
| Identity collision | 256-bit keys (2^128 to collide), full pubkey as storage key |
