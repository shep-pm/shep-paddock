# Slice 2: placements, strays and drift, idle leases, reclaimable leases

The second slice of shep-paddock, settled on 2026-10-05 from the slice 1 design session's "Out" list. Slice 1's spec (`docs/brainstorming/specs/2026-10-04-slice-1-design.md`) still holds everywhere this one is silent. The vocabulary is in `CONTEXT.md`.

Slice 2 is done when this works on the GPU host: laya runs on the GPU while nothing else wants it and in RAM beside qwen; a Strata model someone starts by hand shows up as a stray and is counted; a benchmark that took a lease with `--release-if-idle 30m` and then hung loses its lease half an hour later; and a session that took a reclaimable lease on qwen keeps it warm overnight until a Strata request needs the room.

## Scope

In:

- placements for models on a sheep, chosen when a model loads
- strays, found while the dog runs, not only at startup, and the GPU memory nobody accounts for
- drift: a loaded model measured well above its footprint
- idle-lease signals: traffic and progress notes, `shep paddock note`, and `--release-if-idle`
- reclaimable leases
- issue #2: a loaded model that a reload moves to another sheep stays excluded on the one it runs on
- `docs/handoff.md` folded into a short `docs/prior-art.md`, and deleted

Out, for slice 3 or later:

- placements for ollama models (ollama could run a model on the CPU with `num_gpu = 0`; nothing needs it yet)
- exclusions per placement: an exclusion names a model and covers all its placements
- admission by measured figures: admission stays on declared figures (ADR 0002)
- bare-footprint leases, `revoke`, kelpie as a client

## Placements

A model on a sheep may declare placements instead of a footprint. Each one is a way the model can run, with its own footprint and the sheep fields it needs:

```toml
[paddock.models.laya]
backend = { sheep = "laya" }
url = "http://127.0.0.1:8000"
prefix = "/laya"
key = "…"
ready = { path = "/health", field = "loaded" }
idle = "8h"

[[paddock.models.laya.placements]]
name = "gpu"
vram = "6G"             # 5042 MiB measured with all three checkpoints, 5632 MiB in earlier runs
ram = "2G"              # 1504 MiB resident
script = "/home/<user>/laya/venv-gpu/bin/laya-serve"
env = { LAYA_DEVICE = "cuda", CUDA_VISIBLE_DEVICES = "0" }

[[paddock.models.laya.placements]]
name = "ram"
ram = "5G"
script = "/home/<user>/laya/venv/bin/laya-serve"
env = { LAYA_DEVICE = "cpu", CUDA_VISIBLE_DEVICES = "" }
```

- A placement carries `name`, `vram`, `ram`, and any of `script`, `args` and `env`. The dog sets them with `SetSheepField` and `SetSheepEnv` before the `Restart`, the same way slice 1 sets a model's `args` and `env`.
- Validation refuses: a model with both placements and its own `vram` or `ram`; placements on an ollama model; two placements with one name; placements of one model that set different keys (every placement sets `script` or none does, and likewise `args` and each `env` key), since a key one placement sets would otherwise outlive it into the next; and a placement that can never fit the host. A model fits the host when any one of its placements does.
- Exclusions name models, so `iq3_s` excluding `laya` covers both of laya's placements.
- Clients never see placements. `/v1/models` is unchanged; the status shows the placement of a loaded model.

### Which placement

When a model is to load, the dog tries its placements in the order they are declared:

1. the first placement that fits now, beside everything loading, loaded or evicting;
2. if none does, the first placement for which an eviction set exists under slice 1's rules (reclaimable models only, least recently used first, a batch waiter only evicting models past `grace`).

So laya loads on the GPU when the card is free, and in RAM when qwen holds the card, instead of evicting qwen. Measured on the host: laya on the GPU beside qwen at 64K context either does not fit or pushes 17 to 27% of qwen onto the CPU, and the footprints above already keep the two apart.

A running model is never moved. laya loaded in RAM stays there when the card frees up, until it is unloaded for idleness or eviction. The next load chooses again.

### Restart and discovery

- `state.json` records the placement of each loaded model. A dog that restarts restores it.
- A model found running that the saved state does not place (a stray, or a state file from slice 1) is counted at the largest VRAM and the largest RAM among its placements, since its placement cannot be read from outside.

## Strays

A stray is a model loaded on the host that the dog did not load (`CONTEXT.md`). Slice 1 finds them at startup only. Slice 2 keeps finding them:

- a sheep that comes `online` (from the `process.*` events the dog already follows) without the dog having started it
- a model in ollama's `/api/ps` that the dog did not load, read every 30 s

What a stray counts as:

- a sheep that serves exactly one configured model is that model: `loaded`, reclaimable, at its footprint (the largest placement when it has several). laya running when the dog starts is laya, not an unknown stand-in.
- a sheep that serves several models, or a sheep no model names, is an unknown stand-in at the largest footprint among its models, as slice 1 does.
- an ollama model the config names is that model; any other is an unknown stand-in at the size `/api/ps` reports, as slice 1 does.

A stray is reclaimable, so a waiter may evict it like any other model, and is marked `stray` in the status until it is unloaded. A stray that goes away by itself (its sheep exits, or it leaves `/api/ps`) is forgotten.

### Unaccounted GPU memory

Some GPU memory belongs to nothing the dog can name, such as a stray PyTorch process. The dog reads `nvidia-smi --query-gpu=memory.used` and `--query-compute-apps=pid,used_memory` every 30 s and reports `unaccounted`: the memory in use minus what belongs to processes it can attribute (a sheep's process tree, or ollama's runners).

- Report only. It never enters admission (ADR 0002): declared figures decide.
- Not reported while a model declared `vram = "all"` is loaded, since `all` takes whatever is free and a rootless podman container's GPU process does not descend from its sheep's process (measured: the Strata engine's parent chain runs through `conmon` to `systemd`).
- Without `nvidia-smi` on the host, the field is absent.

## Drift

Drift is a loaded model measured well above its declared footprint (`CONTEXT.md`). It is reported, not refused (slice 1 Q18).

- Measured: a sheep model's VRAM is the sum over its process tree in `--query-compute-apps`, and its RAM is the memory shep reports for the sheep. An ollama model's VRAM is its runner's, matched by the model digest in the runner's command line; ollama's own `/api/ps` size under-reports by about 3 GB (16,475 MiB reported against 19,504 MiB measured for qwen at 64K), so it is not used.
- Drift is a measurement more than 10% above the declared figure, for VRAM or RAM.
- A figure declared `all`, or one with no measurement (a podman sheep's VRAM, an ollama model's RAM), is `unmeasured` and never drifts.
- The status shows each loaded model's measured figures and a `drift` flag. The dog logs one line when a model starts drifting and one when it stops.

Measurement rides the same 30 s survey as strays and unaccounted memory.

## Idle leases

A lease can look idle: held, but nothing seems to use it. Two signals say a lease is in use (slice 1 Q15b), and nothing else:

- a request through the dog for the leased model from the lease holder's own client
- a progress note: `PUT /paddock/leases/{id}` with `{"note": "step 412/900"}` (at most 1024 bytes). For a heartbeat lease the same call renews it. A connection-held lease accepts it too, as a note only.

`shep paddock note "step 412/900"` sends one for the lease in `$PADDOCK_LEASE`, so a command run under `shep paddock run` can report progress from inside. It reads `$PADDOCK_KEY` and `$PADDOCK_URL` like `run` does.

Each lease records `last_activity`, the later of the two signals or its grant. The status shows `idle_for`, and a waiter blocked by a lease says so: "iq2_xs is held by bench-01 since 08:00, idle for 3h".

A lease is released for idleness only when it asked for that: `release_if_idle` in the take body, or `shep paddock run --release-if-idle 30m`. Once its `idle_for` reaches that, the lease ends with the reason `idle`:

- a connection-held lease gets `{"ended": {"reason": "idle", "idle_for": "30m"}}` on its stream, and `shep paddock run` says so on stderr and lets the command run on
- a heartbeat lease's next renewal answers `404`, as for any lease that has ended

## Reclaimable leases

A lease can be reclaimable: `"reclaimable": true` in the take body, or `shep paddock run --reclaimable`. It keeps its model loaded while nothing else needs the room, and gives it up when something does.

- The model it names is reclaimable, not held. It is never named in a refusal and never blocks a waiter.
- While the lease lasts, the model is not unloaded for idleness.
- A waiter may evict it under slice 1's rules: an interactive waiter once its in-flight requests finish, a batch waiter only after `grace` without use.
- When the model is evicted, the lease ends with the reason `reclaimed` (`{"ended": {"reason": "reclaimed"}}` on a stream). The holder takes a new lease if it still wants the model (slice 2 Q8).
- A model named by both a held and a reclaimable lease is held.
- A reclaimable lease waits for its model like any batch lease, and loading it may evict other reclaimable models.
- `state.json` records the flag. A file without it reads as not reclaimable.

## Issue #2: a model moved by a reload

A config reload can point a loaded model at a different sheep. The model keeps running on the one it was loaded on until it unloads, so the book must keep treating that sheep as busy:

- a model's exclusion check compares the sheep each loaded model was loaded on, not only the one its config names now
- a failed load's retry goes through the load gate again, so it cannot start on the new sheep while the old one still runs
- a loaded model the config no longer names keeps its recorded sheep

## Status

`GET /paddock/status` gains, without removing anything:

- per model: `placement` (null for a model with none, or while unloaded), `stray`, `measured` (`{"vram_bytes": …, "ram_bytes": …}`, each null when unmeasured), and `drift`
- per lease: `last_activity`, `idle_for` (seconds), `release_if_idle` (seconds or null) and `reclaimable`
- under `host`: `unaccounted_vram_bytes`, absent when it is not reported

`shep paddock status` prints the placement and drift beside each model, `idle` and `reclaimable` beside each lease, and the unaccounted memory under the host totals.

## Restart

Everything slice 2 adds to a lease or a loaded model is in `state.json`. The file's version goes up by one; a slice 1 file still loads, with no placements, no activity times (a lease's activity starts at the restart) and no reclaimable leases.

## Testing

- The book's property test grows: placements in the generated configs, reclaimable leases among the generated leases, and the invariants that a running model never changes placement, a held model is never evicted, and a reclaimable lease never blocks a waiter.
- Unit tests for placement choice, strays from events and from `/api/ps`, the `nvidia-smi` parsing and attribution (from captured output), drift crossing in both directions, both idle signals, idle release, reclaim, and issue #2.
- One more integration test against a real shepherd: a sheep started by hand turns up as a stray.

## On the host

- laya's GPU placement needs the CUDA venv built on 2026-10-05 at `~/laya/venv-gpu` (PyTorch for CUDA 13).
- The 5 bench models declared at cutover with `vram = "all"` and no measurement get measured figures once drift reporting shows what they use.
