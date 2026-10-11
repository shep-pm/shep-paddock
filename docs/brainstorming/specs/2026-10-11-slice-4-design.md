# Slice 4: turns for requests, turn RAM, and one Strata sheep

The fourth slice of shep-paddock, settled on 2026-10-11. It takes in #24 and three items of #21. The specs for slices 1 to 3 (`docs/brainstorming/specs/`) still hold everywhere this one is silent. The vocabulary is in `CONTEXT.md`, which this slice changes: a turn is now one conversation with one request in flight, and a model with turns keeps a request turn.

Before this slice, one Strata model's settings lived in five places: its sheep's env in the Flockfile, its `[paddock.models.*]` table, Strata's run config for it, a reasoning setting the server writes, and two scripts that patch the run config. Changing how many conversations a model keeps meant editing `sequences` in the paddock and two cache numbers in Strata's config by hand, and nothing kept them in step. On 2026-10-11 iq3_xxs had `sequences = 2`, 4 cache slots and a 6 GiB cache, while its sessions ran 80K to 150K tokens and parked at up to 3.6 GiB each, so the cache held one or two and skipped parking the rest.

Slice 4 is done when this works on the GPU host: changing `sequences` on a Strata model in `dogs.toml` resizes its conversation cache and its footprint with no other edit; a short interactive request to a Strata model that three leases are using is served after the request ahead of it, not after everything already sent; and every Strata model runs on one `strata` sheep whose settings live in its `[paddock.models.*]` table.

## Scope

In:

- requests count against turns, one in flight per turn, with a request turn for requests under no lease (#24)
- `turn_ram`: the RAM one turn holds, counted in the footprint and told to the backend
- the peak measured RAM and VRAM of a loaded model in the status
- a config change reaching a loaded model by reloading it once it is idle
- from #21: why the config watch missed an edit made outside shep, splitting `src/config/tests.rs`, and how `vram = "all"` reads in the status
- on the host: one `strata` sheep, its glue moved out of the Strata clone, and the migration

Out, for later:

- a cache budget the dog grants at load from what its books have free (ADR 0005)
- VRAM per turn: no backend needs it yet
- trying a smaller KV precision for Strata, which would only change `turn_ram`
- the rest of #21

## Turns for requests

A request for a loaded model with `sequences` is admitted against turns. A model with no `sequences` forwards every request at once, as now.

Which turn a request takes:

- `X-Paddock-Lease: <id>` names a lease, with the id as clients see it. When the id is a live lease of the requesting client on that model, and the lease takes a turn, the request takes that lease's turn.
- Otherwise, when the client holds leases on the model that take turns, the request takes the first of those turns that is free. When none is free it waits for one of them, and never takes the request turn, which would push out a parked conversation.
- Otherwise it takes the request turn.
- A header naming a lease that has ended, belongs to another client, or names another model falls through to the rules after it. A command that runs on after its lease is revoked keeps working.

How a turn is served:

- A turn has one request in flight at a time. A request for a turn that already has one waits in the dog's queue, with `reason_kind` `turn` and a reason naming what it waits behind and its place in line: "iq3_xxs request turn is serving mac-sessions, 1 ahead".
- A lease's turn serves its holder's requests first come. The request turn serves interactive requests first, then batch, first come within each, as waiters are served now.
- `X-Paddock-Max-Wait` counts the wait for a turn as it counts the wait for a load. Past it the request gets the `503` it gets today.
- A request that waited for a load or an eviction is admitted against turns once its model is loaded, keeping its place.
- Reclaimable and bare leases still take no turn. A client whose only leases on the model are reclaimable uses the request turn.
- The request turn is not a lease, and `sequences` still counts only leases: a model with `sequences = 2` grants two leases and serves three conversations.

A client that sent several requests at once under one lease now has one in flight and the rest waiting. That is the change kelpie's workers will see: three leases with six in flight become three in flight and three visible waiters. It is why the pull request is `feat!`.

## Turn RAM

A model with `sequences` may declare `turn_ram`, a size in the config's grammar: the RAM one conversation holds while the backend keeps it warm.

```toml
[paddock.models.iq3_xxs]
backend = { sheep = "strata", env = { MODEL = "IQ3_XXS", FAMILY = "qwen", IMAGE = "localhost/strata" } }
container = "strata"
sequences = 2
turn_ram = "4G"
ram = "41G"
vram = "all"
```

- The `ram` above is an estimate, 53 GiB measured with a 12 GiB cache less the cache, until step 5 of the migration measures it.
- Its RAM footprint is `ram + (sequences + 1) × turn_ram`, the one being the request turn. `ram` is the model holding no conversation. Admission, `NeverFits`, drift and the status all use the total.
- On a model with placements, `turn_ram` is added to each placement's RAM.
- `turn_ram` without `sequences` is refused when the config is read, as is `turn_ram` on an ollama model.

## What a sheep is told

Before it starts the sheep of a model with `turn_ram`, the dog parks two env keys on it beside the model's own `env`:

- `PADDOCK_SLOTS`: `sequences + 1`, the conversations the backend should keep
- `PADDOCK_TURN_RAM_MIB`: `turn_ram` in MiB, a whole number, so a shell script can multiply it

A model's own `env` may not set a key starting with `PADDOCK_`, which is refused when the config is read. Models on one sheep must all declare `turn_ram` or none of them, an extension of `SharedSheepMismatch`, since parked keys outlive the model that parked them.

## A changed config reaches a loaded model

A loaded model keeps the settings it was started with, so a config change used to wait for its next idle unload or eviction. Now the dog applies it when nothing would be cut off.

- A loaded model whose launch settings changed in a reload is stale: its sheep, its backend's `env`, `args` or `script`, its placements', `turn_ram`, `sequences` or `container`. A change only to `idle`, `excludes`, `prefix`, `apis` or `name` is not, since the dog applies those without the backend.
- A stale model reloads once no lease holds it, nothing is in flight, nothing waits for it, and it has gone unused for its grace period, so a run of plain requests is not cut between two of them. Reclaimable leases do not hold it back, and survive the reload, since the model comes back. The reload is an unload then a load, as an eviction's are, and a request that arrives meanwhile waits for the load.
- While a model is stale the dog counts the larger of its old and new footprints, figure by figure, so raising a figure counts at once and lowering one counts after the reload.
- The status shows `stale: true` on the model, and its `state` stays what it was until the reload starts.
- The reload starts only once the model fits beside everything else at its new figures. Until then it stays stale, and nothing is evicted for it, since a settings edit should not end someone else's work.
- A stray is never reloaded: the dog did not start it. A dog restart forgets staleness, since `state.json` does not keep launch settings.
- A reload that brings back the settings the model was started with clears `stale` and reloads nothing.

## Peak measured figures

Each loaded model's status gains `measured_peak`, in the shape of `measured`: the highest RAM and VRAM the survey has read for it since it last loaded. It starts again at each load, so a peak from an earlier config never shows under a new one. It is reported and never used for admission (ADR 0002). It is there so measuring a footprint again is reading one line of `shep paddock status`.

## Status

`GET /paddock/status` gains, without removing anything:

- per model: `stale`, `measured_peak`, `turn_ram_bytes` (null when unset), and `turns` (`{"lease": 2, "request": 1}`, null without `sequences`), so the extra conversation the backend keeps for the request turn is visible
- per model's declared footprint: the total with its turns, as admission counts it
- per waiter: a waiting request lists `client`, `priority`, `since`, `reason_kind` and the lease it is under, if any

`shep paddock status` prints the peak beside the measured figures, a model's turns as `2 lease + 1 request`, and prints `vram = "all"` as `all`, not the host's size (#21).

## From #21

- After slice 3's deploy, editing `dogs.toml` on the GPU host logged no reload, and `container` was only picked up after `shep restart paddock`. Find why the config watch missed it and fix it, or say in the README that edits made outside shep need a `shep restart paddock`. The cutover below edits `dogs.toml` live, so this lands first.
- `src/config/tests.rs` (892 lines) splits by what it tests before this slice's config checks go in.
- `vram = "all"` reads as `all` in `shep paddock status`, as it already does in reasons and logs.

## Restart

Waiting requests are not in `state.json`: a request's connection does not outlive the dog. The file's version does not change.

## Testing

- The book's property test generates requests beside leases, and checks that no turn ever has two requests in flight, that a request under a lease never takes the request turn while its lease's turn exists, and that the request turn serves interactive before batch.
- Admission against a fake backend: the header picks the lease's turn, a header for another client's lease falls through, a client with two leases gets two in flight, and a model without `sequences` forwards everything.
- Config: the footprint with `turn_ram`, the refusals above, and placements with `turn_ram`.
- The sheep backend parks `PADDOCK_SLOTS` and `PADDOCK_TURN_RAM_MIB` against a fake shepherd.

## On the host

The Strata glue moves out of the Strata clone into `~/strata-shep/`, with a README saying what each file does and where the settings live:

- `shep-strata.sh` names its container `strata` and, before `podman run`, writes into the model's run config: `--conversation-cache-slots $PADDOCK_SLOTS`, `--conversation-cache-mib` as `PADDOCK_SLOTS × PADDOCK_TURN_RAM_MIB`, the shared server keys (`idle_unload_s: 0`, `reasoning_budget_tokens`, `fit_max_tokens`), and any engine flags in an optional `STRATA_ARGS`. That replaces `apply-strata-config.py`, and survives a setup rerun, which drops engine flags added by hand.
- `strata-context.sh` only reruns setup. Context and KV precision stay Strata setup's, since setup derives other settings from them.
- `Flockfile.toml` holds one `strata` sheep in place of five.
- The `dakota` skill gains a "Where config lives" section.

Migration, in order:

1. Release this slice and install it on the GPU host. Models without `turn_ram` behave as before.
2. Canary, with nothing down: add the `strata` sheep beside the five, and a temporary `[paddock.models.strata-canary]` on it running IQ2_XS with `turn_ram`. Ask it something through the dog, then check the settings line the engine logs at start and the footprint in the status. Remove the canary.
3. Cut over in a window the maintainer picks, with no Strata model loaded and no Strata lease, since `state.json` names each loaded model's sheep. Back up `dogs.toml` and the Flockfile, point the five models at `strata` with their `env`, `turn_ram`, `sequences` and the shared container, and delete the five old sheep.
4. Load each model once and check its logged settings and its footprint.
5. Measure each Strata model's `ram` again from `measured_peak` with an empty cache, since today's figures include whatever the cache held when they were taken.

Rollback is the two backups and `shep add` for the old sheep.

## Follow-ups

- shep-kelpie: send `X-Paddock-Lease` on calls made under a lease, and `X-Paddock-Priority: batch` on worker calls made under none.
- Try iq3_xxs on `k8v4` or `q4_0` KV and see whether quality holds and how far `turn_ram` drops.
