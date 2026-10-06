# Security policy

Report a vulnerability privately, through "Report a vulnerability" on this repository's Security tab, not in a public issue.

Only the latest published version gets fixes.

## What the dog promises

If the dog listens on a network you trust and the client keys stay secret, then only a holder of a key can reach a model or take, renew or release a lease. Every route except `GET /v1/models` needs a key, and a client can only renew or release its own leases.

That holds only while:

- the keys in `[paddock.clients]` stay out of logs, shells and version control
- every backend stays bound to `127.0.0.1`, so the dog is the one way in from the network

## What it does not promise

- No TLS. The dog serves plain HTTP and is for a LAN. Put a proxy in front of it for anything else.
- Anything on the host can still reach a backend's port directly. The dog does not block that. It counts a model it did not load as unknown, at startup only (ADR 0001, `docs/adr/0001-paddock-is-the-only-authority.md`).
- `GET /v1/models` lists model names and their state to anyone who can reach the port.
- An adopted dog runs at the shepherd's own trust level.
