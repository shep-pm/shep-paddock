# Slice 1: one endpoint, footprint admission, model leases

The first slice of shep-paddock, settled in the design session of 2026-10-04. The vocabulary is in `CONTEXT.md` and the decisions behind it are ADRs 0001 to 0003. This file is what the plan is written from.

Slice 1 is done when this works on the GPU host: a benchmark takes a lease on `iq2_xs` with `shep paddock run`, a qwen request arrives an hour later from another machine, and that request is refused at once with a reason naming the benchmark, while the benchmark never notices.

## Scope

In:

- one HTTP endpoint that routes OpenAI and Anthropic requests by `model`, and other APIs by path prefix, streaming responses through unchanged
- two backend kinds: a sheep, and ollama driven through its API
- footprints and exclusions, with admission by declared totals
- waiting: interactive ahead of batch, the grace period, committed eviction, a wait cap with immediate refusal
- model leases, held by an open connection or by heartbeats, and `shep paddock run`
- per-model idle unload
- discovery and the saved lease book across a dog restart
- `/v1/models` and a status endpoint
- per-client bearer keys

Out, for later slices:

- placements. laya is declared with its RAM placement only, which is the only one it has: its venv carries a CPU-only PyTorch (`2.14.1+cpu`), so `LAYA_DEVICE=cuda` still computes on the CPU. A GPU placement needs a CUDA build of PyTorch first, and laya's own Flockfile records that on the GPU it either did not fit beside qwen at 64K context or pushed 17 to 27% of qwen onto the CPU, which contradicts the handoff's "laya plus qwen can share the GPU". Measure that pair before slice 2 declares it
- stray and drift detection, idle-lease signals, `--release-if-idle`, reclaimable leases that keep a model loaded past its idle timeout but give it up to a waiter (slice 2)
- bare-footprint leases, `revoke`, kelpie as a client (slice 3)

## The process

One binary, two modes, the kelpie pattern:

- Started by the shepherd with no arguments, it is the dog. `shep_client::dogs::probe` on the first line of `main`, `ReconnectingClient::connect_as_dog` with the name from `$SHEP_DOG_NAME`, config from its `[paddock]` section in `dogs.toml`, the same as shep-log-rotate.
- Started with arguments, it is the CLI. shep passes `shep paddock <args>` straight to the adopted binary, so `shep paddock run -- <cmd>` and `shep paddock status` need nothing from shep.

The dog does not ask for the shepherd channel in slice 1. It talks to shep only through the socket client: list, start and stop sheep, change a sheep's args or env, and subscribe to `process.*` events to learn when a sheep has exited.

## Config

Everything lives under `[paddock]` in `$SHEP_HOME/dogs.toml`. Figures are from `docs/handoff.md` and from measurements on the GPU host on 2026-10-04. The one marked `measure` is still a placeholder.

```toml
[paddock]
listen = "0.0.0.0:8700"
grace = "2m"            # Q10: how long a reclaimable model must be unused before a batch waiter evicts it
max_wait = "120s"       # Q14: default cap on how long a request waits
reconnect = "60s"       # Q19: how long a connection-held lease survives a dog restart

[paddock.host]
vram = "24564M"         # what nvidia-smi reports as total
ram = "63439M"          # what free -m reports as total

# One key per client. `key` is marked #[shep(secret)].
[[paddock.clients]]
name = "mac-sessions"
key = "…"

[[paddock.clients]]
name = "bench-01"
key = "…"

[paddock.backends.ollama]
kind = "ollama"
url = "http://127.0.0.1:11434"

[paddock.models."qwen3.8:27b"]
backend = "ollama"
name = "qwen3.8:27b-ctx131072"   # what ollama calls it
apis = ["openai"]
vram = "22323M"                  # 21.8 GiB
ram = "4G"                       # measure
idle = "2h"

[paddock.models.iq2_xs]
backend = { sheep = "iq2_xs", env = { CONTEXT = "131072" } }
url = "http://127.0.0.1:8080"
ready = { path = "/health", field = "loaded" }
apis = ["openai", "anthropic"]
vram = "all"
ram = "37G"
idle = "2h"

[paddock.models.iq2_xs-256k]
backend = { sheep = "iq2_xs", env = { CONTEXT = "262144" } }
url = "http://127.0.0.1:8080"
ready = { path = "/health", field = "loaded" }
apis = ["openai", "anthropic"]
vram = "all"
ram = "44G"
idle = "2h"

[paddock.models.iq3_s]
backend = { sheep = "iq3_s" }
url = "http://127.0.0.1:8080"
ready = { path = "/health", field = "loaded" }
apis = ["openai", "anthropic"]
vram = "all"
ram = "55G"
excludes = ["laya"]
idle = "2h"

[paddock.models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
prefix = "/laya"
key = "…"               # laya's own bearer key, which the dog sends in place of the client's
ready = { path = "/health", field = "loaded" }
ram = "5G"              # 4.86 GiB resident with all three checkpoints preloaded
idle = "8h"
```

Notes on the shape:

- A model on a sheep may carry `args` or `env`. Before starting the sheep for that model, the dog sets them with `SetSheepField` and `SetSheepEnv`, which park until the next spawn, and then sends `Restart`, never `Start`, even for a stopped sheep. `Restart` promotes parked fields, and it is how shep's own CLI starts a sheep it already has. Both were tried on the GPU host with a throwaway sheep: parked `args` and `env` reached the next child, stopped or running. Two models on one sheep, like `iq2_xs` and `iq2_xs-256k`, are mutually exclusive by construction, since one sheep runs one process.
- Several Strata models share port 8080. They also share the GPU, so their footprints already keep two of them from loading together.
- `ready` is the dog's own check, polled after the backend starts, because a sheep coming online is not a model being loaded. shep's `online` does not wait for a readiness probe past `listen_timeout` (3 s by default), after which the sheep goes online anyway with a warning, and the Strata container is up well before `/health` reports `loaded`. A `field` is ready when present and truthy, so laya's non-empty `loaded` list counts. laya answers `/health` with the full payload only to its own key, which the dog sends. A model with no `ready` is ready when its sheep is online.
- The Strata launch script hard-codes `CONTEXT=131072` today. It reads `CONTEXT` from the environment as part of the cutover, the same change that removes its laya and ollama handling (ADR 0001).
- `excludes` names models and applies both ways. `iq3_s` excluding `laya` is the same as `laya` excluding `iq3_s`.
- Sizes use shep's `MemSize` grammar, `^\d+(G|M|K)?$` in binary units, and durations its `UpDuration` grammar, `^\d+(ms|h|m|s)?$`. Nothing else is accepted, so a size reads the same here as in a sheep's `max_memory`.
- A config change arrives as a dog-config event. The dog re-reads it, and it applies to the next admission decision. Nothing loaded is unloaded because its figures changed.

Validation at start refuses: models sharing a sheep that set different env keys, or where some set `args` and others do not, since a key one model sets would otherwise outlive it into the next; a model whose footprint can never fit the host, an `excludes` naming an unknown model, a model on an unknown backend, two models with the same `prefix`, and a client with an empty key.

## Requests

Every route needs `Authorization: Bearer <client key>`, except `GET /v1/models`.

| route | how the model is found |
|---|---|
| `POST /v1/chat/completions`, `/v1/completions`, `/v1/embeddings` | `model` in the JSON body, API `openai` |
| `POST /v1/messages` | `model` in the JSON body, API `anthropic` |
| `/<prefix>/...` | the model whose `prefix` matches, any method, the prefix stripped |
| `GET /v1/models` | none: lists every model, with `loaded` and `state` fields added |

Then, in order:

1. Unknown model: `404`, listing the models that exist.
2. The model does not speak this API: `400`, naming the APIs it does speak. No translation (Q20).
3. Loaded and not being evicted: forward now.
4. Otherwise the request becomes a waiter (below). When it is admitted, it is forwarded.

Forwarding:

- The body is passed through unchanged, except that `model` is rewritten to the backend's `name` when one is set, and for ollama any `keep_alive` field is removed (ADR 0001).
- The client's `Authorization` is replaced by the model's `key`, or removed when it has none.
- The response, SSE included, streams back byte for byte, with the backend's status and headers.
- A forwarded request counts as in flight on its model from admission until the response body ends or the client disconnects. A model with requests in flight is never unloaded.

`X-Paddock-Priority: batch` marks a request as batch. `X-Paddock-Max-Wait: <duration>` replaces `max_wait` for that request.

## Admission

The dog keeps one book: each model's state (`unloaded`, `loading`, `loaded`, `evicting`, `unloading`), its in-flight count, its last-used time, and the leases.

A model fits when, counting every model that is `loading`, `loaded` or `evicting`:

- the VRAM totals stay within the host, where `all` means the whole host and fits only beside models that declare no VRAM
- the RAM totals stay within the host
- no exclusion pairs it with one of them

When a waiter's model does not fit, the dog looks for a set of models to evict:

- only reclaimable models: none named by a held lease
- for a batch waiter, only models unused for at least `grace`
- least recently used first, adding models until the waiter fits

If such a set exists, the eviction is committed. Each model in it turns `evicting`, and new requests for it become waiters with the reason "evicting for <model>". Once its in-flight count reaches zero it is unloaded, and once every model in the set is unloaded the waiter's model is loaded. If no set exists, the waiter stays queued with the reason, or is refused (below).

Waiters are served interactive first, then batch, first-come within each. A waiter that cannot be served yet does not block one behind it whose model fits, so a laya request is never stuck behind a queued Strata lease.

Loading:

- sheep backend: set args and env if the model has them, start the sheep, wait for it to come online, then poll `ready`
- ollama backend: `POST /api/generate {"model": <name>, "keep_alive": -1}` with no prompt, which returns once the model is loaded
- the time a load took is remembered per model and used for estimates

Unloading:

- sheep backend: stop the sheep
- ollama backend: `POST /api/generate {"model": <name>, "keep_alive": 0}`

A load that fails or is not ready within its `load_timeout` (default `5m`) is retried once. A second failure fails its waiters with `502` and the backend's error, and the model returns to `unloaded` (Q18).

Idle unload: a loaded model that no lease names, with nothing in flight, unused for its `idle`, is unloaded. There is no default model to restore (Q17).

## Waiting, refusal, estimates

Each waiter carries a reason and, when one can be given, an estimate of when it will be served:

- loading: the model's last load time, minus how long this load has run
- blocked by a lease with an expected end: that end
- blocked by a lease with no expected end: no estimate
- queued behind other waiters: the sum of their estimates, when they all have one

A request waits up to its cap. When it arrives, or whenever its estimate changes, it is refused at once if the estimate is past the cap, or if there is no estimate and the block is a held lease. A refusal is `503` with `Retry-After` when there is an estimate, and a JSON body:

```json
{
  "error": "busy",
  "model": "qwen3.8:27b",
  "reason": "iq2_xs is held by bench-01 since 08:00",
  "expected_until": "2026-10-04T20:00:00Z"
}
```

A request that reaches its cap while waiting gets the same `503`.

Leases do not have a cap unless they ask for one. They wait as long as their connection stays open.

## Leases

`POST /paddock/leases` with a JSON body:

```json
{ "model": "iq2_xs", "priority": "batch", "expected": "8h", "note": "strata h2h run 3", "hold": "connection" }
```

- `priority` defaults to `batch` (Q10). `expected` is optional and used only for estimates (Q15). `max_wait`, when set, refuses the lease like a request.
- `hold: "connection"` (the default): the response is a stream of newline-delimited JSON events, `{"queued": {"reason": …, "estimate": …}}` as often as the reason changes, then `{"granted": {"id": …}}`, then a heartbeat event every 15 s so a dead peer is noticed. The lease lasts while the stream is open. A stream that breaks without a release keeps the lease for `reconnect`, so its holder can attach again with `POST /paddock/leases/{id}/attach`, which answers the same stream. A lease nobody attaches to in that time ends.
- `hold: "heartbeat"` with `ttl` (default `60s`): the dog answers `{"id": …}` once the lease is granted, after waiting like the streamed form. The holder renews with `PUT /paddock/leases/{id}` within each `ttl`, and a missed renewal ends the lease.
- `DELETE /paddock/leases/{id}` releases either kind at once.
- Only the client that took a lease may renew, attach to or release it.

A granted lease loads its model if needed, and the model counts as held from then until the lease ends. A lease on a model that never fits the host is refused at once with `422`.

### `shep paddock run`

```
shep paddock run --model iq2_xs [--expected 8h] [--note "…"] [--interactive] -- <command> [args…]
```

1. Take a connection-held lease, printing each queued reason to stderr.
2. Once granted, run the command with `PADDOCK_LEASE` set to the lease id, and stdin, stdout and stderr inherited.
3. When the command exits, release the lease with `DELETE`, close the stream, and exit with the command's status.
4. If the stream breaks while the command runs, reconnect with the same lease id until `reconnect` runs out. The command is never killed by the wrapper. If the lease is lost, it says so on stderr and lets the command finish.

The wrapper reads its client key from `$PADDOCK_KEY`, and refuses to run without one. It reads the dog's address from `$PADDOCK_URL`, falling back to `http://127.0.0.1:8700`.

`shep paddock status` prints the status endpoint as a table.

## Status

`GET /paddock/status`:

```json
{
  "host": { "vram_bytes": 25757220864, "ram_bytes": 66520760320, "vram_declared_bytes": 25757220864, "ram_declared_bytes": 39728447488 },
  "models": [
    { "model": "iq2_xs", "state": "loaded", "in_flight": 0, "last_used": "…", "held_by": ["bench-01"] }
  ],
  "leases": [
    { "id": "…", "client": "bench-01", "model": "iq2_xs", "since": "…", "expected_until": "…", "note": "strata h2h run 3", "hold": "connection" }
  ],
  "waiters": [
    { "client": "mac-sessions", "model": "qwen3.8:27b", "kind": "request", "priority": "interactive", "since": "…", "reason": "…", "estimate": null }
  ],
  "errors": [ { "model": "iq3_s", "at": "…", "error": "…" } ]
}
```

`errors` keeps the last 20 failed loads. Sizes are bytes, so a script reading the status does no unit parsing.

## Restart

On start, before it listens:

1. Read the saved state from `$SHEP_HOME/paddock/state.json`, written with `shep-core`'s `atomic_file` after every lease change and every load. It holds the leases and which model each sheep was last started for.
2. Find what is loaded:
   - sheep: running sheep from the shepherd's list, then each such model's `ready`
   - ollama: `GET /api/ps`
   - A running sheep is serving the model the saved state says it was started for. shep answers a sheep's env keys but not their values, so the dog cannot read that off the sheep itself.
   - A running sheep with no saved record was started outside the dog. It counts as loaded at the largest footprint of any model on that sheep, reclaimable, and the status endpoint names it as unknown until it is unloaded.
3. Restore leases:
   - heartbeat leases stay, with their `ttl` counted from the restart
   - connection-held leases stay for `reconnect`, and are released if their holder does not reconnect with the lease id in that time
4. A model found loaded that no lease names is reclaimable, with its last-used time set to the restart.

Requests that were in flight when the dog died fail, and their clients retry (Q19).

## Errors in the dog itself

- A sheep backend's sheep exits while loaded (a `process.exit` event the dog did not cause): the dog stops the sheep, so shep's autorestart cannot bring the model back without the dog counting it. The model turns `unloaded`, its in-flight requests fail as the backend closes them, and its leases stay. The next request, or a lease that still names it, loads it again.
- ollama does not answer: its models are reported unavailable, and requests for them fail with `502`, not as waiters.

## Testing

- The book (fit, eviction choice, waiter order, estimates, refusal) is pure and has no I/O, so it is tested by table: given these models, leases and waiters, this is what happens next. Every Q in the design session that settled a behaviour gets at least one case, named after the behaviour.
- Backends sit behind one trait with `load`, `unload` and `probe`. A fake backend drives the dog's HTTP layer in tests, including streaming and slow loads.
- An `integration` feature and CI job build a real shep from `main`, as shep-log-rotate does, and run the dog against a sheep that is a small HTTP stub.
- Loading a real model is never part of CI.

## Crates

- `reqwest` 0.13 for calls to backends.
- `hyper` 1 with `hyper-util` for the server. No shep dog runs an HTTP server yet. shep's own metrics endpoint reads HTTP/1.1 by hand, which is too little for streaming SSE through a proxy, and `reqwest` already brings `hyper` in, so the server adds little to the tree.

## For the plan to settle

- qwen's measured RAM.
