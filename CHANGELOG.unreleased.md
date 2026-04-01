# Unreleased (v0.1.1)

## Features

- Delete contact: remove a contact with P2P notification, persistent queue for offline peers, auto-delete two-person groups, right-click context menu in sidebar
- Group chat sync: pull-based bidirectional sync of missed group messages when peers reconnect, with msg_id deduplication, joined_at filtering for new members, chunked transfer with ACK flow control, and kicked member protection

## Fixes

- Fix infinite handshake loop on simultaneous connection (both peers sending HELLO at the same time caused endless reset cycle)
- Fix IPv6 SLAAC address mismatch in outgoing contact requests (peer responding from a different IPv6 privacy address was treated as unknown; now matched by /64 prefix)
- Fix contact showing offline after accepting friend request (missing online notification and presence exchange post-accept)
- Fix avatar not showing after adding friend (avatar exchange now triggers on both sides after request is accepted)
- Fix stale pending delete firing after re-adding a contact (pending deletes now cleared on friend accept)
- Fix post-call reconnect deadlock (AwaitingIdentity state now resets after jittered timeout)
- Fix crossed-handshake loop on simultaneous ConnectAll (randomized stale timeout per peer)
- Fix chat text overflow in call view (long messages now wrap instead of horizontal overflow)
- Move speaking indicator from right panel to voice channel sidebar (right panel now always shows full member list)
