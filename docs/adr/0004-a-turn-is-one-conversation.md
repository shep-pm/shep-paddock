# A turn is one conversation, and requests take turns

A Strata backend decodes one request at a time behind its own queue, and keeps a bounded number of conversations parked in RAM. Requests the dog forwards at once sit in that queue, where the dog cannot order them or show why they wait: on 2026-10-11 a short interactive request waited about 25 minutes behind three kelpie workers with six requests in flight (#24). So a turn is one conversation with one request in flight, the dog queues past that itself, and requests under no lease share one extra request turn, served interactive first.

## Considered options

- Count requests against `sequences` with no link to leases, as #24 first proposed. Simpler, but a lease holder's requests and a stranger's compete for the same turns, and a one-off request between two holders' requests evicts one of their parked conversations.
- Let a lease holder have any number of requests in flight. On a backend that decodes one at a time it gains nothing, and a second request is usually another conversation that pushes out the holder's own.

## Consequences

- A client that sends several requests at once under one lease sees one in flight and the rest waiting in the status.
- Which lease a request belongs to comes from `X-Paddock-Lease`, falling back to the client's leases on the model, so clients that hold several leases should send it.
