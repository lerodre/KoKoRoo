# Unreleased (v0.1.1)

## Features

- Delete contact: remove a contact with P2P notification, persistent queue for offline peers, auto-delete two-person groups, right-click context menu in sidebar

## Fixes

- Fix infinite handshake loop on simultaneous connection (both peers sending HELLO at the same time caused endless reset cycle)
- Fix IPv6 SLAAC address mismatch in outgoing contact requests (peer responding from a different IPv6 privacy address was treated as unknown; now matched by /64 prefix)
- Fix contact showing offline after accepting friend request (missing online notification and presence exchange post-accept)
- Fix avatar not showing after adding friend (avatar exchange now triggers on both sides after request is accepted)
- Fix stale pending delete firing after re-adding a contact (pending deletes now cleared on friend accept)
- Fix post-call reconnect deadlock (AwaitingIdentity state now resets after 5s timeout)
- Fix chat text overflow in call view (long messages now wrap instead of horizontal overflow)
