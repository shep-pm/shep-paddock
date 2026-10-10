# Slice 3: bare-footprint leases, revoke, and measuring a job's GPU memory

The third slice of shep-paddock, settled on 2026-10-09 from the "Out" list of slice 2. The specs for slices 1 and 2 (`docs/brainstorming/specs/`) still hold everywhere this one is silent. The vocabulary is in `CONTEXT.md`.

Slice 3 is done when this works on the GPU host: `shep paddock run --vram 8G --ram 2G -- ./train.sh` waits for room, evicts a reclaimable model to get it, and shows the job's measured GPU memory in the status; and `shep paddock revoke <id>` from the Mac ends a lease someone forgot, stopping a bare job within its grace period.

## Scope

In:

- bare-footprint leases: memory for a job that runs its own GPU code, with no model server
- `revoke`: an admin client ends someone else's lease
- measuring a bare job's GPU memory through the pid `shep paddock run` gives

Already done, in #17: a per-model `sequences` limit, so a lease past it waits its turn. That is what lets kelpie drop its own GPU lock, which kelpie tracks on its side. The dog needs nothing more for it.

Out, for later:

- placements for ollama models, and exclusions per placement (as in slice 2)
- admission by measured figures: admission stays on declared figures (ADR 0002)
- a hold that watches a pid: a bare lease is held by a connection or a heartbeat, like any lease
- exclusions on a bare lease: one declares memory only, and a job that cannot run beside a model leases that model instead

## Bare-footprint leases

A lease may name a footprint instead of a model:

```json
{"footprint": {"vram": "12G", "ram": "4G"}, "expected": "2h", "note": "fine-tune run 3"}
```

- `footprint` and `model` are exclusive, and one of them is required. `vram` is a size or `"all"`, `ram` a size, in the config's grammar. Either may be left out, as none, but not both.
- A footprint that can never fit on the host is refused with a `400` when it is asked for, as a model that can never fit is refused when the config is read.
- `priority`, `expected`, `max_wait`, `hold`, `ttl` and `note` mean what they mean for a model lease.
- `reclaimable` and `release_if_idle` are refused with a `400`. A bare lease has no model to give back, and its holder's requests do not go through the dog.

How it is admitted:

- It is admitted against the host's totals like a model's load, counting what is already declared, and may evict reclaimable models to make room, under slice 1's rules for a batch or interactive waiter.
- Once granted it is held: never evicted, never unloaded for idleness, and named in the reason of any waiter it blocks.
- It takes no turn, since it names no model.
- While it waits it is told why, as a model lease is: a held model, a grace period, an eviction under way.

How it is named:

- The status lists it among the leases with `"model": null` and its `footprint`, and adds its declared figures to the host's declared totals.
- A reason that names it says `lease 12 of bench-01 (12G VRAM, 4G RAM)` where a model lease would name the model.

`shep paddock run --vram 12G --ram 4G -- <command>` takes one, held by its connection as `run --model` is. `--model` and `--vram`/`--ram` are exclusive.

## Measuring a bare job

`shep paddock run` sends its child's pid in the take body as `"pid": 4321`. The dog uses it only when the request comes over loopback, since a pid from another host names nothing on this one. Otherwise it ignores it.

- Each survey adds up the GPU memory `nvidia-smi` gives for that pid and every process descended from it, the way it finds a sheep's processes, and reports it as the lease's `measured` VRAM.
- A bare lease measured more than 10% above its declared VRAM shows `drift`, as a model does. That is reported and never acted on (ADR 0002).
- Memory measured for a bare lease is not unaccounted. A bare lease with no pid has its declared VRAM taken off the unaccounted figure, down to zero.

## Revoke

A client marked `admin = true` in its `[[paddock.clients]]` entry may revoke any lease:

```toml
[[paddock.clients]]
name = "mac-sessions"
key = "…"
admin = true
```

- `POST /paddock/leases/{id}/revoke`, with an optional body `{"reason": "forgotten since Tuesday"}` (at most 1024 bytes). A client that is not an admin gets a `403`. A lease that has already ended, or never existed, is a `404`, as for a release.
- `shep paddock revoke <id> [--reason …]` sends it, with `$PADDOCK_KEY` and `$PADDOCK_URL` as the other commands do.
- The lease ends with the reason `revoked`, and the line on its stream names who and why: `{"ended": {"reason": "revoked", "by": "mac-sessions", "note": "forgotten since Tuesday"}}`. A heartbeat holder's next renewal answers `404`.
- The status keeps no history of ended leases, so the dog's log is where a revoke is recorded: who revoked which lease, its holder, and the reason.

What happens to the work under a revoked lease:

- A model lease's command runs on, as after an idle end. The model stays loaded and becomes reclaimable unless another lease holds it, and `shep paddock run` says on stderr that the lease was revoked, by whom and why.
- A bare lease's job holds the memory itself, so `shep paddock run` stops it: `SIGTERM` to the child, then `SIGKILL` once `--grace` has passed (30s by default), and it exits with the child's status. The dog keeps the footprint counted until `run`'s connection closes, so nothing loads into memory the job still holds.
- A bare lease held by a heartbeat has no `run` to stop its job, so its footprint is freed at the revoke, and the status says the lease was revoked while its holder may still be running.

## Status

`GET /paddock/status` gains, without removing anything:

- per lease: `footprint` (`{"vram_bytes": …, "ram_bytes": …}`, null for a model lease), `measured` (as for a model, null when unmeasured or not bare), and `drift`
- per lease, `model` is null for a bare lease

The status lists no clients, so `admin` appears nowhere in it.

`shep paddock status` prints a bare lease's footprint in place of its model, with its measured VRAM and drift beside it.

## Restart

A bare lease is in `state.json` with its footprint and its pid, and counts from the moment the dog starts again. A connection-held one starts detached, as any does, so `run` attaching again within the reconnect window keeps it. The file's version goes up by one; an older file still loads, with no bare leases.

## Testing

- The book's property test generates bare leases beside model leases, and checks that the memory they declare is counted until they end, that a held bare lease is never evicted, and that a revoked one's memory is counted until its holder detaches.
- `run` against a fake dog: a revoked bare lease sends `SIGTERM`, then `SIGKILL` after the grace, to a child that ignores the first; a revoked model lease leaves its child running.
- The pid is honoured over loopback and ignored otherwise, and the survey adds up a pid's descendants against a recorded `nvidia-smi` and `/proc`.

## On the host

- The maintainer marks `mac-sessions` as `admin` in `~/.shep/dogs.toml` on the GPU host.
- A bare `run` of a short CUDA job on the GPU host shows it waiting for a held model, evicting a reclaimable one, and its measured VRAM in the status.
- A revoke from the Mac of a bare `run` with a job that ignores `SIGTERM` stops it after the grace.
