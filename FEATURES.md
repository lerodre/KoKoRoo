# hostelD — Features

P2P encrypted voice + chat + file transfer + screen sharing. No servers. No accounts. Direct IPv6 UDP.

---

## Encryption & Security

- **E2E encryption on everything** — Voice, chat, files, screen sharing encrypted with ChaCha20-Poly1305. Nothing travels in plaintext.
- **X25519 key exchange** — Ephemeral keypairs per session. Shared secret derived via Diffie-Hellman.
- **Identity-bound authentication** — For known contacts, session key upgraded with identity DH: `SHA-256(ephemeral_DH || identity_DH || "hostelD-msg-key")`. Proves private key ownership. Prevents impersonation even if attacker knows a contact's public key.
- **PKT_MSG_CONFIRM handshake** — After IDENTITY exchange, both peers send CONFIRM encrypted with the upgraded key. Successful decrypt = identity verified. New contacts (TOFU) stay on ephemeral-only key.
- **Per-packet counter nonce** — Each packet gets a unique nonce. Prevents replay attacks.
- **TOFU trust model** — Trust peer on first contact. If their key changes later, show a warning.
- **Key change detection** — Different pubkey from same contact triggers red warning screen.
- **Nickname spoofing detection** — If a new key claims a known nickname, warn the user.
- **Same-address impersonation detection** — Unknown key from known contact's IP triggers warning.
- **Verification code** — 8-hex-digit code (XXXX-XXXX) derived from shared secret. Both peers see the same code. Verify verbally to detect MITM.
- **Encrypted chat history on disk** — Saved chats encrypted with key derived from identity secret. Stored at `~/.hostelD/chats/{id}.enc`.
- **Identity key stored locally** — 64-byte file at `~/.hostelD/identity.key`. Permissions 0600 (user-only). Never leaves the device.
- **Fingerprint** — Short identifier (hD-XXXXXXXX) derived from public key. Used to recognize contacts.

## Firewall & Rate Limiting

- **Per-IP rate limiter** — More than 1000 packets/sec from a single IP = strike.
- **Auto-blacklist** — 5 strikes = IP banned automatically.
- **Auth failure tracking** — Failed decryption counts as a strike. Protects against garbage floods.
- **Manual IP ban/unban** — Block or unblock specific IPs from settings.
- **OS firewall integration** — Automatically creates UDP allow rule on call start (ufw on Linux, netsh on Windows, macOS prompts natively).

## Voice Calls

- **Full-duplex voice** — Simultaneous send and receive. Real conversation, not walkie-talkie.
- **Opus codec** — 48kHz mono, 64kbps, 20ms frames. Low latency, high quality.
- **RNNoise suppression** — AI-based noise removal applied to mic input before encoding.
- **System audio capture** — Share desktop audio during calls. Uses WASAPI loopback (Windows), PipeWire Monitor (Linux), ScreenCaptureKit (macOS).
- **System audio device selection** — Pick which output device to capture audio from.
- **Stereo-to-mono auto-conversion** — Handles stereo-only devices transparently.
- **Mic mute toggle** — Mute/unmute mic during call without dropping the connection.
- **Ring buffer audio pipeline** — Lock-free SPSC buffers. No blocking in audio callbacks.
- **Minimal latency** — Speaker buffer capped at 200ms to keep conversation natural.
- **Hangup signal** — Clean disconnect with encrypted HANGUP packet (sent 3x for reliability).

## Screen & Webcam Sharing

- **Screen sharing** — Real-time screen capture encoded with VP8, split into UDP chunks, encrypted.
- **4 quality presets** — 720p/2Mbps, 1080p/4Mbps, 1080p/6Mbps, 1080p60/8Mbps.
- **Multi-monitor selection** — Pick which display to share.
- **Webcam sharing** — Share camera feed instead of screen. V4L2 (Linux), MediaFoundation (Windows), AVFoundation (macOS).
- **Wayland support** — Uses XDG Desktop Portal + PipeWire on Wayland (not just X11).
- **Chunk assembly** — Large VP8 frames split into 1300-byte UDP chunks with reassembly headers.
- **Keyframe forcing** — VP8 emits keyframe every ~3 seconds for fast recovery.
- **Auto-scaling** — Scales to target resolution but never upscales. Nearest-neighbor.
- **Chunk pacing** — 200us between UDP chunks to avoid burst loss.

## Messaging (Background Daemon)

- **1:1 encrypted messages** — Send text messages to contacts without an active call. Encrypted end-to-end.
- **Persistent chat history** — Messages saved per-contact in encrypted files. Survives app restart.
- **Chat history deletion** — Delete all messages for a contact permanently.
- **Delivery acknowledgement (ACK)** — Sent messages confirmed as received by peer.
- **Outbox with retry** — Failed messages queued and retried with exponential backoff (10s, 30s, 1m, 5m, 15m cap).
- **In-call chat** — Send and receive text during active voice calls.
- **Address auto-update** — When a contact connects from a new IP, `last_address` is updated on disk so reconnection always uses the latest known address.

## File Transfer

- **Drag & drop file send** — Drop a file onto a chat conversation to send it to the contact.
- **Offer/Accept/Reject workflow** — Recipient chooses to accept or reject each file.
- **Chunked UDP transfer** — Files split into 4KB encrypted chunks with AIMD congestion control.
- **SHA-256 integrity verification** — File hash verified on completion to ensure no corruption.
- **Missing chunk retransmission (NACK)** — Receiver detects gaps and requests retransmission of specific chunks.
- **Transfer cancel** — Either side can cancel mid-transfer.
- **Progress tracking** — Real-time progress updates (bytes transferred / total) shown in UI.
- **Chat integration** — File transfer messages appear inline in chat history with filename and size.
- **Drop zone indicator** — Visual feedback when dragging a file over the conversation.

## Contact Requests

- **Send contact request** — Send request to a peer by IP:port. They receive it in their Requests tab.
- **Accept request** — Accept saves the contact on both sides. Both can now message each other.
- **Reject request** — Silently discard. Sender not notified.
- **Block request** — Ban the sender's IP and pubkey permanently.
- **Auto-accept known contacts** — If someone you already have as a contact sends a request, it's auto-accepted.

## Connection & Reconnection

- **Auto-connect on startup** — Daemon connects to all known contacts when the app launches. Staggered (1 every 100ms) to avoid network burst.
- **Periodic reconnect beacon** — Every 10 minutes, daemon retries connecting to any contact that went offline.
- **Fresh address on beacon** — Beacon reloads contacts from disk before retrying, so it always uses the latest IP from IP relay updates.
- **Keepalive packets** — Sent every 60 seconds to all connected peers. Keeps NAT mappings alive.
- **Peer timeout detection** — No activity for 5 minutes = peer marked offline.
- **Socket yield/reclaim** — Voice calls and messaging share the same UDP port. Daemon releases the socket during a call and reconnects all peers when the call ends.
- **Session resumption after call** — Daemon remembers who was connected before the call and reconnects them automatically.
- **HELLO retry with backoff** — Handshake retried up to 20 times (every 3 seconds) before giving up.
- **IP migration cleanup** — When a contact reconnects from a new address, the stale session at the old address is removed.

## Presence

- **Online/Away/Offline status** — Three states. Broadcast to all connected peers via encrypted presence packet.
- **Away auto-detection** — If no mouse movement or keyboard activity for 15 minutes, status changes to Away. Returns to Online on any input.
- **Mouse tracking** — `last_mouse_move` updated on any pointer delta.
- **Keyboard tracking** — `last_key_press` updated on any input event.
- **Presence display** — Green circle = Online. Yellow circle = Away. Grey circle = Offline. Shown in message list and chat header.
- **Presence on connect** — When a peer finishes handshake, your current presence is sent immediately.

## IP Relay & Peer Discovery

- **IP announce** — Daemon announces own IP to connected peers every 30 minutes (or on change).
- **IP change detection** — Detects when your public IP changes and re-announces to all peers.
- **Peer query** — Ask connected peers for another contact's current address. "Find Peer" button in contact detail view.
- **Peer response** — If you know a queried contact's address, relay it back.
- **Announce caching** — Received IP announcements cached for 2 hours.
- **Query rate limiting** — Max 6 inbound queries per minute per peer. Outbound queries limited to 1 per 5 min per target.
- **IPv6 privacy extension handling** — Recognizes that peers may use different addresses in the same /64 subnet. Matches by prefix.

## Contacts

- **Contact list** — Saved per-peer as JSON in `~/.hostelD/contacts/`. Keyed by full pubkey hex (no collisions).
- **Contact info** — Stores fingerprint, nickname, pubkey, last IP, last port, first seen, last seen, call count.
- **Contact search/filter** — Search by nickname or fingerprint in contact list.
- **Contact blocking** — Block a contact's pubkey + IP. Appears with strikethrough in list.
- **Contact deletion** — Remove contact and their chat history.
- **Quick dial** — Click a contact to auto-fill their IP/port and start a call.
- **Contact detail view** — Full info panel with Call, Message, Find Peer buttons.
- **Call counter** — Tracks how many times you've called a contact. Gauge of trust.
- **Deterministic contact ID** — Both peers derive the same contact ID: SHA-256(sorted(pubkey_A, pubkey_B)).

## Privacy

- **No central server** — All communication is direct peer-to-peer.
- **No accounts** — No registration, login, email, or phone number. Identity is a local keypair.
- **IP censoring in UI** — IPs hidden by default (e.g., "2803:****"). Toggle to reveal.
- **No telemetry** — No analytics, metrics, or tracking of any kind.
- **Local-only logging** — Debug logs written to `~/.hostelD/hostelD_log.txt`. Never sent anywhere.

## UI

- **Desktop GUI** — eframe/egui. Works on Windows, macOS, Linux.
- **Terminal UI (TUI)** — Crossterm-based. Arrow-key navigation, text input. For headless or terminal-only systems.
- **Sidebar tabs** — Profile, Messages (with Add Friend), Groups, Call, Settings, Colors, Logs.
- **Unread badge bubbles** — Red circular badges with count on sidebar tabs and contact list entries.
- **In-call UI** — Verification code, peer info, mute button, screen share controls, video preview, chat panel.
- **Theme system** — 18 customizable color properties. Smart randomize generates harmonious palettes.
- **Video fullscreen** — Toggle screen share viewer to fullscreen.
- **Error screen** — Shows error message with return-to-setup button.
- **Desktop notifications** — OS-native notifications for incoming calls (notify-send on Linux, osascript on macOS, PowerShell on Windows).
- **Notification sound** — Plays sound on incoming message. 3-second cooldown to avoid spam.
- **Ringtone** — Plays ringtone loop on incoming call. Stops on accept/reject/dismiss.
- **Drag & drop files** — Drop files onto conversation to send. Visual drop zone indicator on hover.

## Cross-Platform

- **Windows** — WASAPI audio, MediaFoundation webcam, DXGI screen capture, netsh firewall, PowerShell notifications, winmm ringtone. Console flash hidden with CREATE_NO_WINDOW flag.
- **Linux** — ALSA/PipeWire audio, V4L2 webcam, X11/Wayland screen capture, ufw firewall, notify-send notifications, gst-play ringtone.
- **macOS** — CoreAudio, AVFoundation webcam, CGDisplayStream screen capture, app-based firewall, osascript notifications, afplay ringtone. ScreenCaptureKit for system audio (macOS 13+).
- **Per-platform Cargo.toml** — Three variants (`.linux`, `.macos`, `.windows`) with correct dependencies for each OS.

## CLI Modes

- `hostelD` — Launch GUI (default).
- `hostelD tui` — Launch terminal UI.
- `hostelD call <ip> <port> <local-port>` — Direct voice call without GUI.
- `hostelD devices` — List available audio devices.
- `hostelD mic-test` — Mic loopback test (hear yourself).
- `hostelD listen <port>` — UDP echo server (network debug).
- `hostelD send <ip> <port> <msg>` — Send UDP packet and wait for reply (network debug).

## Packet Protocol

50+ packet types across voice, messaging, file transfer, and groups:

| Type | Name | Use |
|------|------|-----|
| `0x01` | HELLO | Voice key exchange |
| `0x02` | VOICE | Encrypted Opus audio |
| `0x03` | IDENTITY | Identity pubkey + nickname (voice) |
| `0x04` | CHAT | In-call chat message |
| `0x05` | HANGUP | Call disconnect |
| `0x06` | SCREEN | VP8 screen share chunk |
| `0x07` | SCREEN_STOP | Screen share ended |
| `0x10` | MSG_HELLO | Messaging key exchange |
| `0x11` | MSG_IDENTITY | Identity exchange (messaging) |
| `0x12` | MSG_CHAT | Text message |
| `0x13` | MSG_ACK | Delivery confirmation |
| `0x14` | MSG_BYE | Messaging disconnect |
| `0x15` | MSG_REQUEST | Contact request |
| `0x16` | MSG_REQUEST_ACK | Request accepted |
| `0x17` | MSG_IP_ANNOUNCE | IP relay announcement |
| `0x18` | MSG_PEER_QUERY | Peer address lookup |
| `0x19` | MSG_PEER_RESPONSE | Peer address reply |
| `0x1A` | MSG_PRESENCE | Online/Away status |
| `0x1B` | FILE_OFFER | File transfer offer |
| `0x1C` | FILE_ACCEPT | Accept file |
| `0x1D` | FILE_REJECT | Reject file |
| `0x1E` | FILE_CHUNK | File data chunk |
| `0x1F` | FILE_ACK | All chunks received |
| `0x20` | FILE_COMPLETE | Sender done |
| `0x21` | FILE_CANCEL | Cancel transfer |
| `0x22` | FILE_NACK | Request missing chunks |
| `0x23` | MSG_CONFIRM | Identity-bound key upgrade confirmation |

## Data Layout

```
~/.hostelD/
  identity.key                      # 64 bytes: 32 secret + 32 public (0600)
  settings.json                     # Nickname, devices, port, theme, banned IPs
  contacts/{pubkey_hex}.json        # One file per contact
  chats/{contact_id}.enc            # Encrypted 1:1 chat history
  chats/grp_{group_id}_{ch}.enc     # Encrypted group chat history
  groups/{group_id}.json            # Group metadata + members
  avatars/own.png                   # Own avatar
  avatars/{contact_id}.png          # Contact avatars
  avatars/group_{group_id}.png      # Group avatars
  files/{contact_id}/               # Received files
  files_tmp/                        # In-progress transfers
  hostelD_log.txt                   # Debug log
```
