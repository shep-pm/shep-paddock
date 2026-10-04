# Handoff: what shep-paddock is for, and what is already known

Written at bootstrap, before any code. The next agent writes the spec and the code from here. Delete or fold this into `CONTEXT.md` and `docs/adr/` once those exist.

## Why it exists

The maintainer's GPU host, dakota, runs several model servers that cannot all be loaded at once. Today nothing arbitrates between them: on 2026-10-04 an unrelated agent session tried to use qwen several times while an eight-hour benchmark held the GPU with a Strata model. Every client has to know what is loaded and what it would break.

shep-paddock is a shep dog on dakota that owns that decision. Clients name the model they want at one endpoint; the dog leases the GPU and RAM, starts and stops the right sheep, queues what does not fit, and protects work that holds a lease.

## The host

- dakota: RTX 4090 (24 GB VRAM), 62 GB RAM, Fedora-based immutable OS (Bluefin), rootless podman with CDI GPU passthrough. The desktop runs on the iGPU, so the 4090 is free for models.
- shep 0.12.0 runs there as a user service. `shep list` today: sheep `iq2_xs`, `iq3_xxs`, `iq3_s` (fold `strata`, autostart off) and `laya`; dog `log-rotate` (adopted).
- ollama is a system-level systemd unit (`ollama.service`), not a sheep. `OLLAMA_KEEP_ALIVE=2h`.

## The consumers

| consumer | how it runs | footprint (measured) | API |
|---|---|---|---|
| Strata IQ2_XS | sheep `iq2_xs`: `~/Strata/shep-strata.sh` runs a podman container publishing 127.0.0.1:8080 | about 23.7 GB VRAM (expert cache fills what is free); 37 GB RAM in use at 128K context, 44 GB at 256K | OpenAI and Anthropic compatible, no API key, `/health` reports `loaded` and `max_context` |
| Strata IQ3_XXS | sheep `iq3_xxs`, same script | about 23.9 GB VRAM; 47 GB RAM at 256K | same |
| Strata IQ3_S | sheep `iq3_s`, same script; the script stops laya first | about 55 GB RAM at 128K, 53 GB at 256K with laya stopped | same |
| qwen3.8:27b | ollama, `qwen3.8:27b-ctx131072` alias | 21.8 GB VRAM at 128K context; spills to RAM (and becomes unusably slow) above that | ollama and OpenAI compatible on 11434 |
| laya | sheep `laya`, port 8000, bearer key | small ModernBERT classifier; runs on GPU or in system RAM | custom JSON API (`state` plus `questions`), not OpenAI |

Speeds, for cold-start and eviction costs: Strata writes 105 to 148 tokens a second depending on the variant; ollama writes 89 tokens a second for qwen3.8:27b at 128K. A Strata model takes under a minute to load. Full measurements are in the maintainer's kelpie-lab repository, `strata/data/engine.json` and `strata/data/headtohead/h2h.json`.

## Coexistence rules (from the maintainer)

- laya plus qwen3.8:27b can share the GPU.
- laya in system RAM plus a smaller Strata model (IQ2_XS, IQ3_XXS) is probably fine.
- Any Strata model plus qwen3.8:27b is never allowed.
- IQ3_S does not allow laya to run at all.

These are hand-written today. Expect more consumers later (image or audio models), so the rule format has to grow.

## What today's ad-hoc arbitration looks like (all of it should move into the dog)

- Strata's `before_load` hook, `~/Strata-data/hooks/unload-ollama.py`, unloads ollama's models before a Strata model loads.
- `~/Strata/shep-strata.sh` stops laya before IQ3_S and restarts it after.
- Strata unloads itself after `idle_unload_s` = 7200; ollama after `OLLAMA_KEEP_ALIVE=2h`.
- shep-kelpie's `gpu` lease (see below) is a lock the maintainer's qwen scripts take on the Mac. It covers only clients on that Mac.
- The `local-llm` skill still says qwen runs on a Windows box via ollama. That stale pointer is why the colliding session looked in the wrong place.

## Requirements

1. One endpoint. Clients name a model; the dog routes by the `model` field for OpenAI (`/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models`) and Anthropic (`/v1/messages`) requests, and by path prefix for non-OpenAI services like laya. Responses stream through unchanged (SSE).
2. Start, stop and swap backends to satisfy a request, following the coexistence rules, and wait for the backend to be healthy before forwarding.
3. Queue a request that cannot be served now (backend loading, or blocked by a lease), with a timeout, and say why it is waiting.
4. Leases for work that is not one HTTP request: acquire (queued, with a timeout), renew (heartbeat), release, and expiry when the holder stops renewing or its process dies. A benchmark that holds a Strata model for eight hours must not be evicted by a qwen request; the qwen request waits, or is refused with the reason and an estimate.
5. Every in-flight HTTP request holds an implicit short lease, so a backend is drained before it is stopped.
6. Idle unload: release a backend nobody has used for a while. Consider "reclaimable" leases (keep it loaded while idle, give it up only when someone else needs the room).
7. Priorities: interactive requests ahead of batch work, FIFO within a level. Decide preemption deliberately (probably never preempt a held lease; maybe preempt reclaimable ones).
8. Visibility: `/v1/models` lists every model and whether it is loaded; a status endpoint lists leases, the queue and why each waiter waits.
9. A command-line wrapper for jobs: run a command under a lease, renewing while it runs, releasing on exit.
10. Reachable from the maintainer's other machines over the LAN, behind a bearer key. Backends stay bound to 127.0.0.1.

## Prior art (researched 2026-10-04) and why this is hand-rolled

No existing tool combines a single model endpoint, coexistence rules, start and stop of arbitrary services, and leases for non-HTTP jobs.

- **llama-swap** (mostlygeek/llama-swap, MIT, Go, releases weekly) is the closest. Its `matrix` router declares which models may run together with a small expression language (`&`, `|`, `()`, `+ref`) and evicts by `evict_costs`; requests queue FIFO during swaps; `ttl` unloads idle models; `/upstream/<model>/` passes non-OpenAI APIs through and triggers swaps. It has no leases, no RAM or VRAM numbers (issue #1004 asks for them), and wants to own the backend processes, which duplicates shep. Rejected as the base; copy its set syntax and solver semantics (read `docs/config.example.yaml`, the `routing.router.settings.matrix` section).
- **reslock** (PyPI): VRAM, RAM and CPU leases in a JSON state file, dead holders cleaned by PID, priority queue, reclaimable leases preempted by higher priority. No service control. Copy the reclaimable-lease idea.
- **worker-q** (samsundar989/worker-q): admission by declared RAM, VRAM and CPU against live free memory; refuses jobs that can never fit at submit time; reports a wait reason. Copy both.
- **gpu-broker** (emergenthq-net/gpu-broker): swaps systemd units and containers, FIFO with interactive requests ahead of batch, restores a default model when idle. One model at a time. Copy the interactive-first queue and the idle restore.
- **jobd** (musharna/jobd): VRAM, RAM and CPU bin-packing with SIGTERM, grace and SIGKILL preemption.
- **aivyx-broker**: an `acquire`/`release` GPU lock with no heartbeat and a 900 s reap. A crashed holder wedges it. The cautionary example for requirement 4.
- **vLLM sleep mode**: offload weights to RAM and wake in seconds. Only for vLLM, but a `sleep`/`wake` hook on a backend is worth leaving room for.
- Ruled out as too heavy or wrong shape for one box: GPUStack, LocalAI, Slurm, HTCondor, Nomad, k3s with Kueue, HAMi, Triton, KServe ModelMesh, Ray Serve. LiteLLM and olla route but do not manage resources; ollama and LM Studio manage only their own engines.

Measuring: NVML (`nvidia-smi --query-compute-apps`, or pynvml's `nvmlDeviceGetComputeRunningProcesses`) gives per-PID VRAM, but under rootless podman PIDs may not map to containers. Use total used and free for admission, per-PID only for attribution.

## shep APIs to build on

Read shep-log-rotate before writing code: it is the reference dog. `shep/docs/dogs.md` and `shep/docs/specs/shep-v1.md` sections 6 to 8 are the contract.

- A dog is an adopted binary (`shep adopt shep-paddock`, `shep enable paddock`) that the shepherd supervises like a sheep, with restart backoff and log capture, but wildcard selectors (`shep stop all`) never reach it.
- `shep_client::dogs::probe` on the first line of `main`, `connect_as_dog`, the `dog_config` attribute from `shep-macros` on the config struct (it marks credential fields), and config under `[paddock]` in `$SHEP_HOME/dogs.toml`.
- `shep-client`: the Unix-socket client with handshake and version-skew handling, typed requests to start, stop and list sheep, and `subscribe` returning an `EventStream` (`process.*` for state changes and readiness, `daemon.*`). Use events rather than polling to know when a sheep is up or has died.
- Readiness and liveness probes (HTTP, TCP, exec) already exist on sheep; the dog can wait on readiness instead of its own health loop, where the sheep defines one.
- `shep-core`: `atomic_file` and `file_lock` for the dog's saved state.
- shep 0.12 gives an adopted dog a shepherd channel when its `--version` answer asks for one (`shep-channel: true`); shep-kelpie's ADR 0004 shows how.
- An `integration` cargo feature plus a CI job that builds a real shep from `main`, as shep-log-rotate does.
- What shep does not have: dependencies between sheep, conflicts or mutual exclusion, or resource limits beyond `max_memory`. All of that is this dog's job. A gap that is generic to shep becomes a shep issue (shep-kelpie's ADR 0001 sets that rule).

## Existing work to extract: shep-kelpie's leases

shep-kelpie (`~/GitHub/shep-kelpie`, shep-pm/shep-kelpie) already holds leases, and the plan is to move that logic here and make kelpie a client.

- `src/lease/` (about 3,800 lines): `book.rs`, `counted.rs` (a counted lease: `cargo-test` admits 3 at once, the rest queue), `gpu.rs` (the `gpu` lock), `door.rs` (the `lease.sock` door), `wire.rs`, `saved.rs`, `window.rs`, and `cli/`.
- The `gpu` lease is a `mkdir` lock at `$TMPDIR/qwen-review/gpu.lock` holding `pid`, `what` and `session` files, the same lock the maintainer's qwen scripts take. A lock whose pid is gone is stale. It reads ollama's `/api/ps` to detect CPU spill and GPU Prometheus metrics through `gpu_metrics_url`.
- Commands: `shep kelpie lease run|take|return|status`.
- Docs: kelpie's `CONTEXT.md` ("Lease"), `docs/design-log.md`, and `docs/adr/0004-the-adopted-kelpie-is-the-lease-dog.md`.
- Two lease authorities for one GPU would conflict. Once paddock serves leases, kelpie's `gpu` lease and the qwen scripts move to it.

## Open questions for the spec

- Rules as explicit sets (llama-swap style), as RAM and VRAM budgets, or both (budgets with sets as overrides)?
- ollama is a system unit, not a sheep. Does paddock unload models through ollama's API (`keep_alive: 0`), or does ollama become a user sheep?
- laya on GPU and laya in RAM: one consumer with two modes, or two consumers?
- Do the Strata sheep keep `shep-strata.sh`, and do its laya and ollama side effects get removed so paddock is the only one deciding?
- Preemption: never for a held lease? For reclaimable ones only?
- Auth: one bearer key, or one per client so the status endpoint can say who holds what?
- Where kelpie's lease engine lands: lifted whole, or rewritten around the new rules?
