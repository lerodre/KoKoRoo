# hostelD — Feature Checklist

## v0.1 (Done)

- [x] X25519 key exchange + ChaCha20-Poly1305 encryption
- [x] Opus voice codec (64kbps, 48kHz mono)
- [x] Verification code (anti-MITM)
- [x] Persistent identity (keypair stored on disk)
- [x] Contact system (save peers after calls)
- [x] Encrypted chat (in-call text messages)
- [x] Encrypted chat history (local storage)
- [x] GUI (eframe/egui)
- [x] TUI (terminal interactive menus)
- [x] CLI (direct call command)
- [x] Firewall (rate limiting + auto-blacklist)
- [x] Settings persistence (mic, speakers, port, nickname)
- [x] Mutual hang up (PKT_HANGUP, both peers disconnect)
- [x] Nickname system (sent during identity exchange)
- [x] Display format: `nickname #fingerprint`
- [x] Contact list in GUI (click to auto-fill peer info)
- [x] TOFU — Trust On First Use (key change warnings)
- [x] Call count tracking per contact
- [x] Full pubkey hex as storage key (collision-proof)
- [x] Migration from old fingerprint-based contact files
- [x] IPv6 address copy button (share your IP easily)

---

## v0.2 (Planned)

### Profile Pictures
- [ ] Allow users to set a small avatar/profile picture
- [ ] Store as base64 in settings or as a separate image file in `~/.hostelD/`
- [ ] Send profile picture hash during identity exchange
- [ ] Transfer full image on first connection or when changed
- [ ] Display in contact list and in-call screen
- [ ] Size limit: 64KB max, resize to 128x128

### Chat History Review
- [ ] Browse past chat histories from the setup screen
- [ ] Select a contact → see full chat history (decrypted from local storage)
- [ ] Scrollable, searchable message list
- [ ] Show timestamps, message direction (sent/received)
- [ ] Option to export chat as plaintext
- [ ] Option to delete chat history for a contact

### IPv6 Sharing Improvements
- [ ] "Share my info" button: copies `[ipv6]:port` to clipboard in one click
- [ ] QR code generation for IPv6+port (for phone/tablet scanning)
- [ ] Connection string format: `hostelD://[ipv6]:port` (click to connect)
- [ ] Show connection string in a share dialog

### Contact Management
- [ ] Rename contacts locally (override peer nickname)
- [ ] Delete contacts
- [ ] Block contacts (refuse connections from specific pubkeys)
- [ ] Contact notes (add personal notes to a contact)
- [ ] Sort contacts by: last seen, name, call count
- [ ] Search/filter contacts

---

## v0.3 (Future)

### Group Calls
- [ ] Multi-peer voice calls (mesh topology for small groups)
- [ ] Group chat rooms
- [ ] Group identity and membership management

### File Transfer
- [ ] Send files over the encrypted channel
- [ ] Progress indicator
- [ ] File type detection and preview

### Push-to-Talk
- [ ] Hold-to-talk mode (alternative to always-on mic)
- [ ] Configurable hotkey

### Audio Improvements
- [ ] Noise suppression (RNNoise or similar)
- [ ] Echo cancellation
- [ ] Volume control per peer
- [ ] Audio level indicators (visual mic meter)
- [ ] Configurable bitrate (32k/64k/96k)

### Network
- [ ] NAT traversal (STUN/TURN for IPv4 compatibility)
- [ ] Connection relay for peers behind symmetric NAT
- [ ] Bandwidth adaptation (lower quality on poor connections)
- [ ] Connection quality indicator

### Security Enhancements
- [ ] Key pinning: permanently pin a contact's key (reject changes without manual override)
- [ ] Identity export/import (backup your keypair)
- [ ] Password-protected identity file (encrypt identity.key with a passphrase)
- [ ] Verified contacts: mutual verification marks contact as "verified"
- [ ] Disappearing messages (auto-delete after N days)

### UX
- [ ] System tray / notifications
- [ ] Dark/light theme toggle
- [ ] Keyboard shortcuts reference
- [ ] Incoming call detection (listen mode + notification)
- [ ] Call history log (time, duration, peer)
- [ ] Multiple language support

---

## Known Limitations

- IPv6 only (no IPv4 NAT traversal yet)
- No incoming call notification (both peers must initiate)
- Chat history is per-contact_id (64-bit truncated hash — extremely unlikely but theoretically possible collision)
- No key revocation mechanism (if your key is stolen, you must create a new identity)
- Fingerprint display is 32 bits (cosmetic collisions possible at ~65K users, does not affect security)
