# Prior art

No existing tool combines one model endpoint, rules for which models can run together, starting and stopping arbitrary services, and leases for work that is not one request. So the dog is hand-rolled. This is what was looked at in October 2026 and what each tool left behind.

## Tools looked at

- llama-swap is the closest. Its `matrix` router declares which models may run together with a small expression language and evicts by cost, requests queue in order during swaps, and a `ttl` unloads idle models. It has no leases, no RAM or VRAM figures, and wants to own the backend processes, which duplicates shep. Rejected as the base. The dog took its idea of sets and its eviction semantics.
- reslock keeps VRAM, RAM and CPU leases in a JSON state file, cleans up dead holders by pid, and lets higher priority take back a reclaimable lease. It has no service control. The dog took the reclaimable lease.
- worker-q admits a job by its declared RAM, VRAM and CPU against live free memory, refuses a job that can never fit when it is submitted, and says why a job waits. The dog says why a waiter waits, and refuses a footprint that can never fit when it reads the config, so a lease on such a model is refused at once. It admits on declared figures too, but against the host's declared totals and what it has already counted, never live free memory (ADR 0002).
- gpu-broker swaps systemd units and containers, puts interactive requests ahead of batch, and restores a default model when idle. It runs one model at a time. The dog took the interactive-first queue.
- jobd bin-packs VRAM, RAM and CPU and preempts with SIGTERM, a grace period and SIGKILL. The dog took the grace period.
- aivyx-broker is an acquire and release GPU lock with no heartbeat and a 900 s reap, so a crashed holder wedges it. It is the example of what a lease without a heartbeat costs.
- vLLM sleep mode moves weights to RAM and wakes in seconds. It only works for vLLM, but a backend may one day want a sleep and wake hook, so the design leaves room.
- Ruled out as too heavy or the wrong shape for one box: GPUStack, LocalAI, Slurm, HTCondor, Nomad, k3s with Kueue, HAMi, Triton, KServe ModelMesh and Ray Serve. LiteLLM and olla route but manage no resources.

## Measuring

NVML (`nvidia-smi --query-compute-apps`) gives VRAM per pid. But a rootless podman container's GPU process does not descend from its sheep: the Strata engine's parent chain runs python, conmon, systemd. So the dog admits on declared figures (ADR 0002) and uses the per-pid figures only to attribute.

## shep-kelpie's leases

The lease engine lives in shep-kelpie's `src/lease/`, about 3,800 lines. Its files are `book.rs`, `counted.rs` (a lease that admits several holders at once), `gpu.rs`, `door.rs` (the `lease.sock` door), `wire.rs`, `saved.rs`, `window.rs` and `cli/`.

The `gpu` lock is a `mkdir` lock at `$TMPDIR/qwen-review/gpu.lock`, holding `pid`, `what` and `session` files. A lock whose pid is gone is stale. kelpie's docs on it are `CONTEXT.md` (the entry for lease), `docs/design-log.md` and `docs/adr/0004-the-adopted-kelpie-is-the-lease-dog.md`.

Two lease authorities for one GPU would conflict, so kelpie becomes a paddock client for the GPU (ADR 0003).
