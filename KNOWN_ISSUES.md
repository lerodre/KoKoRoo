# Known Issues & Security Weaknesses

Known issues, platform bugs, and security weaknesses in KoKoRoo.

---

## Platform Issues

### macOS: Screen/webcam video transmission is corrupted

Video frames arrive corrupted on the receiver side when sharing screen or webcam from macOS. Likely related to the BGRA->I420 color conversion or frame stride differences on Apple Silicon. Needs investigation.

**Status:** Open

---

## Security Weaknesses

### 1. identity.key is the local master key

**Risk: HIGH**

The file `~/.kokoroo/identity.key` (64 bytes: 32 secret + 32 public) controls everything:
- Decrypts all locally stored chats (`.enc` files)
- Allows impersonating the user in calls and messages (X25519 keypair)
- Derives the local storage encryption key via `crypto::derive_storage_key`

**Impact:** If someone copies this file, they have full access to the user's identity and can read all chat history.

**Mitigation:**
- Unix: file permissions `0600` (owner-only)
- Windows: depends on NTFS folder permissions
- Full disk encryption (BitLocker/LUKS) protects against physical access

**Limitation:** Desktop OSes (Windows/Linux/macOS) cannot restrict per-app file access. Any process running as the user can read the file.

---

### 2. 1:1 calls — MITM without verification

**Risk: MEDIUM**

The call handshake uses ephemeral X25519 without identity authentication. A network attacker can MITM if users don't verify the `XXXX-XXXX` code.

**Attack flow:**
1. Attacker intercepts `HELLO` packets from both peers
2. Establishes two separate sessions (one with each peer)
3. Relays audio between both sessions
4. Each peer has a different shared secret, but if they don't compare the code, they won't detect it

**Mitigation:** Verify the `XXXX-XXXX` code verbally at the start of the call. If it matches, MITM is impossible (shared secrets would differ with an attacker in the middle).

**Limitation:** Verification is manual and optional. Many users skip it.

---

### 3. Groups — Shared symmetric key

**Risk: MEDIUM**

All group members share the same `group_key` (256-bit ChaCha20-Poly1305). Any member can:
- Decrypt all group call audio
- Read all group chat messages
- View screen share and webcam from other members

**Impact:** Group security is only as strong as its weakest member.

**Mitigation:** Carefully select group members.

---

### 4. Key rotation on member kick

**Risk: MEDIUM**

When a member is kicked, the `group_key` is rotated. However:

#### 4a. Vulnerability window
Between the kick and all members receiving the new key, the kicked member can still decrypt traffic encrypted with the old key. The `previous_key` is kept as fallback for peers that haven't updated yet.

#### 4b. Offline members
If a member is offline during rotation:
- They continue using the old key until they receive the update (via `PKT_GRP_UPDATE`)
- The system tries to decrypt with the previous key as fallback
- The update is delivered when the member connects and a peer with the new key is online

#### 4c. Only offline + kicked member online
If the only online peer is the kicked member and another outdated member:
- Both have the old key and can temporarily communicate
- There is no way to prevent this without a central server
- When a peer with the new key appears, the outdated member will receive the rotation

#### 4d. No forward secrecy in groups
If the kicked member recorded packets encrypted with the old key, they can always decrypt them. Rotation only protects **future** traffic.

#### 4e. Split-brain with simultaneous kicks
If two admins kick two different members at the same time, both generate rotations with `key_version` N+1. Partially resolved by `key_version` (highest version wins on update), but if both generate the same version, the last update received overwrites the previous one.

---

### 5. Unauthenticated GRP_HELLO

**Risk: LOW**

The `GRP_HELLO` packet (to join group calls) is not encrypted — it contains a `group_id` and a dummy pubkey. An attacker who knows the `group_id` can send HELLOs.

**Limited impact:**
- Without the `group_key`, they cannot decrypt voice/chat packets
- Without the `group_key`, they cannot send valid packets (ignored)
- The leader may register them as a peer if the IP matches a member, but they won't appear in the UI

**Mitigation:** The `group_id` is a random 128-bit value, hard to guess.

---

### 6. Invite distribution

**Risk: MEDIUM**

The `group_key` travels in a `PKT_GRP_INVITE` packet encrypted with the 1:1 E2E session between inviter and invitee. If that 1:1 session was compromised by MITM (see point 2), the attacker gets the `group_key`.

**Mitigation:** Verify the contact (code `XXXX-XXXX`) at least once before inviting them to a group.

---

### 7. Malicious member re-shares the key

**Risk: LOW (non-technical)**

An active group member could copy the `group_key` and share it outside KoKoRoo with unauthorized people. This is a human trust problem, not a technical one.

**Mitigation:** Only invite trusted people. Kick and rotate key if leakage is suspected.

---

### 8. Local chats accessible to any user process

**Risk: MEDIUM**

The `.enc` files in `~/.kokoroo/chats/` are encrypted with ChaCha20-Poly1305, but the key is derived from `identity.key` which is in the same directory.

**Impact:** Any process running as the user can read `identity.key`, derive the storage key, and decrypt all chats.

**Mitigation:** Full disk encryption + don't run untrusted software.

---

### 9. Visible network metadata

**Risk: LOW**

KoKoRoo uses UDP over IPv6. Although content is encrypted, a network observer can see:
- That two IPs are communicating
- When a call starts and ends (from packet patterns)
- Approximate group size (from packet volume)
- That screen sharing is active (larger and more frequent packets)

**Mitigation:** Use a VPN or overlay network (like Tailscale/ZeroTier, which KoKoRoo already supports for IPv6).

---

### 10. Rate limiting bypass

**Risk: LOW**

The firewall (`firewall.rs`) implements rate limiting (>1000 pkt/sec = strike, 5 strikes = ban). An attacker could stay just below the threshold to send unwanted traffic without being banned.

**Limited impact:** Without the session key, packets are discarded on decryption failure.

---

### 11. No key rotation on voluntary leave

**Risk: LOW**

Currently `group_key` rotation only happens when an admin **kicks** a member. If a member leaves voluntarily, the key is not rotated.

**Reason:** A member who leaves voluntarily is unlikely to be malicious. But they could still decrypt traffic if they intercept packets.

**Possible future improvement:** Also rotate key on voluntary leaves.

---

### 12. group.json not encrypted on disk

**Risk: MEDIUM**

Group files (`~/.kokoroo/groups/{id}.json`) are stored as plaintext JSON. They contain:
- The `group_key` in hex format
- Member list with pubkeys, nicknames, IPs
- Channel configuration

**Impact:** Filesystem access exposes all group keys.

**Mitigation:** Full disk encryption. Possible future improvement: encrypt group files with `identity.key`.

---

### 13. No expiration for previous_key

**Risk: LOW**

The `previous_key` is kept indefinitely as fallback for outdated peers. A kicked member who captures packets encrypted with the new key cannot decrypt them, but the `previous_key` allows decrypting traffic from peers that are slow to update.

**Possible future improvement:** Expire `previous_key` after a reasonable period (e.g., 24 hours).

---

### 14. Call button uses default port on first click

**Risk: UX bug**

When clicking the call button from the sidebar, the first click sometimes dials `[::]:9000` (default) instead of the contact's actual IPv6 address. Clicking the contact a second time loads the correct address and the call works.

**Likely cause:** The call screen reads the peer address before the GUI has resolved/loaded it from the contact data.

**Workaround:** Click the contact once to open the chat, then click the call button.

**Status:** Open

---

## Priority Summary

| # | Issue | Risk | User mitigation |
|---|-------|------|-----------------|
| 1 | identity.key exposed | HIGH | Full disk encryption |
| 12 | group.json plaintext | MEDIUM | Full disk encryption |
| 2 | MITM without verification | MEDIUM | Verify XXXX-XXXX code |
| 3 | Shared symmetric group key | MEDIUM | Select members carefully |
| 4 | Key rotation window | MEDIUM | Inherent to P2P |
| 6 | Compromised invite | MEDIUM | Verify contact first |
| 8 | Local chats accessible | MEDIUM | Full disk encryption |
| 9 | Network metadata | LOW | VPN |
| 5 | Unauthenticated GRP_HELLO | LOW | Random group_id |
| 7 | Key re-sharing | LOW | Trust |
| 10 | Rate limit bypass | LOW | Session key required |
| 11 | No rotation on voluntary leave | LOW | Future improvement |
| 13 | previous_key no expiration | LOW | Future improvement |
| 14 | Call button uses default port on first click | UX | Click contact twice |
