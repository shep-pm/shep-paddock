# Security policy

Report a vulnerability privately, through "Report a vulnerability" on this repository's Security tab, not in a public issue.

Only the latest published version gets fixes.

## What the dog promises

If the dog listens on a network you trust and the client keys stay secret, then only a holder of a key can reach a model or take, renew or release a lease. Every route except `GET /v1/models` and `GET /api/tags` needs a key, and a client can only attach to, renew or release its own leases. Another client's lease answers `404`, as an unknown id does.

A client's credential headers stop at the dog. `Authorization`, `X-Api-Key`, `Cookie` and any `Proxy-*` header are never forwarded to a backend. A model with a `key` gets that key as its `Authorization` instead. The query string and the body are forwarded as sent, so a key a client puts there reaches the backend.

That holds only while:

- the keys in `[paddock.clients]` stay out of logs, shells and version control
- every backend stays bound to `127.0.0.1`, so the dog is the one way in from the network

## What it does not promise

- No TLS. The dog serves plain HTTP and is for a LAN. Put a proxy in front of it for anything else.
- Anything on the host can still reach a backend's port directly. The dog does not block that. It counts a model it did not load as unknown, at startup only (ADR 0001, `docs/adr/0001-paddock-is-the-only-authority.md`).
- `GET /v1/models` lists model names and their state to anyone who can reach the port, and `GET /api/tags` lists the names of the models on ollama's API.
- Lease ids are not secrets. They are sequential, and `GET /paddock/status` shows any client with a key every lease's id, model and holder. A `503` turning a request away from a held model names who holds it too, since saying who holds what is the dog's job.
- An adopted dog runs at the shepherd's own trust level.
