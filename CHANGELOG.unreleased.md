# Unreleased (v0.1.1)

## Fixes

- Fix infinite handshake loop on simultaneous connection (both peers sending HELLO at the same time caused endless reset cycle)
- Fix IPv6 SLAAC address mismatch in outgoing contact requests (peer responding from a different IPv6 privacy address was treated as unknown; now matched by /64 prefix)
- Fix contact showing offline after accepting friend request (missing online notification and presence exchange post-accept)

## Features

- Delete contact (pending)
