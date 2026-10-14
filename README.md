# shep-paddock

[![crates.io](https://img.shields.io/crates/v/shep-paddock.svg)](https://crates.io/crates/shep-paddock)
[![License](https://img.shields.io/crates/l/shep-paddock.svg)](https://github.com/shep-pm/shep-paddock#license)

A shep dog that leases one host's GPU and RAM to model servers and jobs, behind a single endpoint.

A dog for [shep](https://github.com/shep-pm/shep) on a host where several model servers share one GPU. Clients name a model at one endpoint; the dog starts and stops the sheep that serve it, following rules about which models can run together, queues requests that do not fit yet, and lets long jobs hold a lease so nothing evicts them.

## Use

```sh
cargo install shep-paddock
shep adopt shep-paddock
```

The dog asks for the shepherd channel, and the shepherd stops it with a message rather than a signal; a dog adopted before this version gets the channel at the next `shep adopt shep-paddock`.

Then add a `[paddock]` section to `dogs.toml` in `$SHEP_HOME`. This one serves a model from a sheep called `llama`:

```toml
[paddock]
listen = "0.0.0.0:8700"

[paddock.host]
vram = "24564M"
ram = "63439M"

[[paddock.clients]]
name = "bench"
key = "change-me"

[paddock.models.llama]
backend = { sheep = "llama" }
url = "http://127.0.0.1:8080"
ready = { path = "/health" }
apis = ["openai"]
vram = "20G"
idle = "2h"
```

- The sheep must already be in the flock. `shep add ./serve.sh --name llama` registers it without starting it.
- A change made through shep applies at once, and an edit made to `dogs.toml` outside shep applies within 30 seconds.
- A change to a sheep model's launch settings, its backend `env`, `args` or `script`, a placement's `env`, `args` or `script`, `sequences`, `turn_ram` or `container`, reaches a model that is already loaded once nothing would be cut off: no lease holds it, no request is in flight on it, no request or lease waits for it, it has gone unused for the `grace` period (120s by default), and it fits at its new figures. The dog then unloads it and loads it again, keeping its reclaimable leases. Until then it counts for the larger of its old and new figures, and the status marks it `STALE`. Other settings, such as `idle`, `excludes`, `prefix` and `apis`, apply at once without a reload, and an ollama model has no launch settings.
- Sizes are `1G`, `512M` or `64K`, durations are `120s`, `5m` or `2h`.
- A model with `vram = "all"` takes the whole GPU. `excludes` names models that cannot load beside it, and models on one sheep never load together.
- A model on ollama points `backend` at a `[paddock.backends.*]` entry of `kind = "ollama"`. The dog removes `keep_alive` and `options.num_ctx` from what it forwards there, because it decides what stays loaded and the model's `name` fixes its context. For the same reason an ollama request that only asks to unload a model gets a `403`. Responses come back as the backend sent them, so their `model` field carries the backend's `name`, not the name the client asked for.
- A sheep model may set `name` too, for a server that knows the model by another name. A request's `model` is rewritten to it, and nothing else in the body changes.
- `apis` says which routes reach a model: `openai` (`/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`), `anthropic` (`/v1/messages`), and `ollama` (`/api/chat`, `/api/generate`, `/api/embed`, `/api/embeddings`).
- `docs/brainstorming/specs/2026-10-04-slice-1-design.md` has every field.
- A model on a sheep may declare `[[paddock.models.<name>.placements]]` instead of `vram` and `ram`. Each placement has a `name`, `vram`, `ram` and any of `script`, `args` and `env`. The dog tries them in order when the model loads and never moves a running one. This one runs on the GPU when it fits and falls back to RAM:

  ```toml
  [[paddock.models.laya.placements]]
  name = "gpu"
  vram = "6G"
  ram = "2G"
  script = "/path/to/venv-gpu/bin/laya-serve"
  env = { LAYA_DEVICE = "cuda", CUDA_VISIBLE_DEVICES = "0" }

  [[paddock.models.laya.placements]]
  name = "ram"
  ram = "5G"
  script = "/path/to/venv/bin/laya-serve"
  env = { LAYA_DEVICE = "cpu", CUDA_VISIBLE_DEVICES = "" }
  ```

- `docs/brainstorming/specs/2026-10-06-slice-2-design.md` has every field of slice 2.
- `sequences` says how many leases a model's backend serves at once: `1` for a server that runs one sequence and caches one prompt, its slot count for a llama-server. A lease past it waits its turn, with a reason naming the holders and its place in line. Each turn has one request in flight, and the rest of its holder's requests wait. Requests under no lease, and under a reclaimable one, share one more request turn. Bare and reclaimable leases take no turn. Unset means no limit, and every request is forwarded at once.
- Three request headers steer a request, and none reaches the backend. `X-Paddock-Priority: batch` queues it behind interactive requests (`interactive` is the default). `X-Paddock-Max-Wait: 90s` replaces the config's `max_wait` for that request. `X-Paddock-Lease: L12` names the lease whose turn the request takes, for a client that holds several leases on a model and needs to say which conversation a request belongs to. It counts when the lease is live, the client's, on the requested model, and not reclaimable. If that turn is busy the request waits for it and no other turn. Any other lease id is passed over, and the request takes a turn as if the header were absent. A value that is not a lease id as the lease routes show it is `400` with `bad_lease`.
- `admin = true` on a `[[paddock.clients]]` entry lets that client revoke a lease, unless a protected client other than itself holds it. `protected = true` keeps a client's leases from every other client's revoke, admin or not.
- A model on a sheep may name `container = "<podman container name>"` when its sheep starts a podman container. The dog measures the container's processes with the model's, counts a container running without its model as a stray, and stops the container after its sheep:

  ```toml
  [paddock.models.iq3_xxs]
  backend = { sheep = "iq3_xxs" }
  container = "strata-qwen-iq3_xxs"
  ```

- A model with `sequences` may declare `turn_ram`, the RAM one conversation holds while its backend keeps it warm, in the same size grammar as `ram`. The backend keeps `sequences + 1` conversations, one per lease turn and one for the request turn, and the footprint counts all of them: `ram + (sequences + 1) × turn_ram`. `ram` is then the model holding no conversation. On a model with placements, `turn_ram` is added to each placement's `ram`. Admission, the fit check at load and the status all use the total. The sheep is told the numbers before it starts, as `PADDOCK_SLOTS` (`sequences + 1`) and `PADDOCK_TURN_RAM_MIB` (`turn_ram` in MiB, rounded up), set after the model's own `env`. `turn_ram` without `sequences`, or on an ollama model, is refused when the config is read. Here the footprint is 41G + 3 × 4G = 53G:

  ```toml
  [paddock.models.iq3_xxs]
  backend = { sheep = "strata", env = { MODEL = "IQ3_XXS", FAMILY = "qwen", IMAGE = "localhost/strata" } }
  container = "strata"
  sequences = 2
  turn_ram = "4G"
  ram = "41G"
  vram = "all"
  ```

- A sheep model may set `metrics = "strata"`, which says its backend is Strata. While the model is loaded, the dog reads its `GET {url}/metrics` at each report, with the model's `key` as the bearer when it has one, and sends the figures to the metrics dog labelled by model. Strata's own numbers would otherwise vanish with each model swap, and its `/metrics` is JSON. `metrics` on an ollama model is refused when the config is read. It is not a launch setting, so changing it never makes a loaded model `STALE`. The series are under Metrics.

Clients send `Authorization: Bearer <key>` to the one endpoint. A request names its model in the body, or reaches it through the model's `prefix`. If the model is not loaded the request waits while the dog frees room and starts it. `GET /v1/models` lists the models and needs no key, and `GET /api/tags` lists the ones on ollama's API the same way.

Hold a model for a long job with `shep paddock run`. The lease lasts until the command exits, and nothing evicts the model meanwhile:

```sh
export PADDOCK_KEY=change-me
shep paddock run --model llama --expected 8h -- ./benchmark.sh
```

`PADDOCK_URL` sets the dog's address and defaults to `http://127.0.0.1:8700`. Without `PADDOCK_KEY` the key comes from shep's secret store, so on a host running shep it can be set once with `shep secret set PADDOCK_KEY --stdin`.

Two flags change how long the lease lasts. `--release-if-idle 30m` ends it once the lease's own client has sent the model no request through the dog, and no note has come, for that long. Other clients' requests do not count, and a request still running keeps the lease in use. `--reclaimable` keeps the model loaded without holding it, until something else needs the room. Inside the command, `shep paddock note "step 412/900"` says the lease is in use.

A job that runs its own GPU code can lease memory instead of a model:

```sh
shep paddock run --vram 12G --ram 4G -- ./train.sh
```

It waits like any lease and may evict a reclaimable model for the room. Nothing evicts it. Run on the GPU host with `$PADDOCK_URL` naming a loopback address, as the default does, the dog measures the job's GPU memory. The command runs in its own process group, so it should not read the terminal. If an admin revokes it, the whole group gets `SIGTERM`, then `SIGKILL` after `--grace` (30s), and the lease is held until every process in the group is gone.

An admin client's key ends a lease with `shep paddock revoke L12 --reason "forgotten since Tuesday"`, unless a protected client other than itself holds it. A model lease's command runs on. A bare lease's command is stopped. The dog's log records who revoked what and why.

A waiter's reason comes with `reason_kind`, one word a client can match on: `loading`, `evicting`, `draining`, `grace`, `held`, `behind` or `turn`. It is on the lease stream's `queued` lines, in a `503` body and in the status.

## Status

`shep paddock status` prints `GET /paddock/status`, which needs a key. It prints the host's totals and declared footprints, each model's state, the leases, the queue, and the last 20 failed loads, as tables. Sizes are in binary units (KiB, MiB, GiB). The JSON endpoint reports them in bytes.

Slice 2 adds columns. The models table shows each model's `PLACEMENT`, and `DRIFT` says yes when it measures more than 10% above what it declared. The leases table shows how long each lease has been `IDLE` and whether it is `RECLAIMABLE`. When the survey can read the GPU, a line under the host table gives the GPU memory that nothing the dog knows of holds, as unaccounted VRAM. The JSON carries more: whether a model is a stray, loaded by something other than the dog, and the figures measured for it. `docs/brainstorming/specs/2026-10-06-slice-2-design.md` names every field.

Slice 3 adds more. A bare lease shows its footprint in place of its model, its measured VRAM (`MEASURED`) and `DRIFT`. `REVOKED-BY` names who revoked a bare lease while its job may still run.

Slice 4 adds a `FOOTPRINT` column to the models table, the VRAM and RAM each model counts for against the host. `all` stands for a model that takes whatever VRAM is free. A bare lease's footprint reads the same way, and a waiting bare lease shows its footprint in place of its model in the waiters table. The JSON gives each footprint as bytes with `vram_all`, and `vram_bytes` keeps the host's VRAM for `all`.

Slice 4 also adds `MEASURED`, `PEAK` and `TURNS` to the models table. `MEASURED` is what the last survey read the model holding, and `PEAK` the highest RAM and VRAM any survey has read since the model last loaded, each as `{vram} VRAM, {ram} RAM` with `-` for a figure not measured. The peak restarts at each load, so one from an earlier config never shows under a new one, and it is reported only: admission and `DRIFT` ignore it. `TURNS` reads `2 lease + 1 request` for a model with `sequences = 2`, the extra slot being the request turn the backend keeps, and `-` without `sequences`. The JSON adds `measured_peak`, `turn_ram_bytes` (null when unset) and `turns` to each model.

Slice 4 also adds `LEASE` to the waiters table, the lease whose conversation a waiting request is in, as `L12`, so a slow answer shows why. It reads `-` for a request on the request turn, a request on a model without turns, a request waiting for the first of several of its client's leases to free, and a lease waiter. The JSON gives it as `lease` on each waiter, null in those cases.

Slice 4 also adds `STALE` to the models table: `yes` for a loaded model whose launch settings the config has since changed, `-` otherwise. A stale model keeps its `loaded` state until the reload starts, then `STALE` goes back to `-` as the state moves to `unloading`. The JSON adds `stale` to each model.

## Metrics

The status shows the queue only when it is asked. The dog also reports three gauges to the shepherd every 15 seconds, so the host's metrics endpoint keeps their history:

- `paddock_model_loaded{model}`: 1 while the model is loaded, 0 in any other state. Every model the status lists is reported, strays included.
- `paddock_leases{model}`: granted leases on the model. A revoked bare lease still listed is not counted. A bare lease counts with no `model` label.
- `paddock_waiters{model, reason_kind}`: requests and leases waiting, by the word the status gives as `reason_kind`. A request waiting for its turn counts under `turn`, and a waiter not yet told a reason under `none`. A bare lease waiter has no `model` label.

A sheep model marked `metrics = "strata"` also has its backend read while it is loaded, each series labelled `model`. Counters come from Strata's `totals`, and Strata sets them back to 0 when it restarts, which the metrics dog reads as a counter reset:

- `paddock_strata_requests_total`, `paddock_strata_prompt_tokens_total`, `paddock_strata_reused_tokens_total`, `paddock_strata_drafts_offered_total` and `paddock_strata_drafts_accepted_total`.
- `paddock_strata_prompt_seconds_total` and `paddock_strata_decode_seconds_total`: Strata's `prompt_ms` and `decode_ms` in seconds.

Gauges come from `live` and `hardware`:

- `paddock_strata_queued` and `paddock_strata_tokens_per_second`, which reads 0 while Strata is not generating.
- `paddock_strata_state{model, state}`: 1 for the state Strata reports among `reading`, `generating`, `idle` and `unloaded`, 0 for the others, and 0 for all four when Strata reports a word not among them.
- `paddock_strata_gpu_utilization_percent`, `paddock_strata_gpu_memory_used_bytes`, `paddock_strata_gpu_temperature_celsius` and `paddock_strata_gpu_power_watts`.

A field Strata leaves out or sends as `null` has no sample, and a field the dog does not know is ignored. A scrape that is refused, takes more than 2 seconds, answers anything but 200 or answers something that is not Strata's JSON sends no Strata series that round, and the dog logs one line for the model and no more until a scrape of it succeeds. The dog's own series above are sent first, so a stuck backend never delays them. When the model stops being loaded or marked, its Strata gauges are sent once at 0 like the label sets above. Its counters are not, since a counter that falls reads as a reset.

A label set that was reported and is gone, because its count fell to nothing or its model left the config, is sent once more at 0 and then dropped. The metrics dog keeps a series' last value until the dog restarts, so without that zero the old count would show on.

This needs a shep whose metrics dog serves channel samples. Under any other, or a dog started by hand, nothing is reported.

## Security

`SECURITY.md` says what the dog promises and what it does not.

## License

MIT OR Apache-2.0, at your option.
