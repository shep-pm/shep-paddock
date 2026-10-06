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

Clients send `Authorization: Bearer <key>` to the one endpoint. A request names its model in the body, or reaches it through the model's `prefix`. If the model is not loaded the request waits while the dog frees room and starts it. `GET /v1/models` lists the models and needs no key, and `GET /api/tags` lists the ones on ollama's API the same way.

Hold a model for a long job with `shep paddock run`. The lease lasts until the command exits, and nothing evicts the model meanwhile:

```sh
export PADDOCK_KEY=change-me
shep paddock run --model llama --expected 8h -- ./benchmark.sh
```

`PADDOCK_URL` sets the dog's address and defaults to `http://127.0.0.1:8700`.

Two flags change how long the lease lasts. `--release-if-idle 30m` ends it once nothing has used the model through the dog, and no note has come, for that long. `--reclaimable` keeps the model loaded without holding it, until something else needs the room. Inside the command, `shep paddock note "step 412/900"` says the lease is in use.

## Status

`shep paddock status` prints `GET /paddock/status`, which needs a key. It prints the host's totals and declared footprints, each model's state, the leases, the queue, and the last 20 failed loads, as tables. Sizes are in binary units (KiB, MiB, GiB). The JSON endpoint reports them in bytes.

Slice 2 adds to it. Each model shows its placement, whether it is a stray (loaded by something other than the dog), its measured figures, and drift when it uses more than it declared. Each lease shows its idle time and whether it is reclaimable. The host shows the GPU memory that nothing the dog knows of holds, as unaccounted. `docs/brainstorming/specs/2026-10-06-slice-2-design.md` names every field.

## Security

`SECURITY.md` says what the dog promises and what it does not.

## License

MIT OR Apache-2.0, at your option.
