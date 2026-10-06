# shep-paddock

A shep dog that owns one host's GPU and RAM: it decides which models are loaded, and who may hold them, so no client has to know what is running or what it would break.

## Language

### What runs

**Model**:
What a client names when it asks for work, such as `qwen3.8:27b`, `iq2_xs` or `laya`. Footprints and exclusions belong to models.
_Avoid_: backend, server, sheep (when the thing a client names is meant)

**Backend**:
What loads and unloads a model: a sheep, or ollama. One backend may serve several models.
_Avoid_: upstream, engine, server

**Placement**:
One way a model can run, each with its own footprint, such as laya on the GPU or laya in system RAM. Clients never see placements; the dog picks one when it starts the model, and a running model is never moved.
_Avoid_: mode, variant

**Footprint**:
The VRAM and RAM a model, or one placement of it, declares it uses. A model whose use grows into whatever is free declares all of it.
_Avoid_: size, budget, requirement

**Exclusion**:
A pairing of models that may never be loaded together even though their footprints fit.
_Avoid_: conflict, set

**Drift**:
A loaded model measuring well above its declared footprint. It is reported, not refused.

**Stray**:
A model loaded on the host that the dog did not load. Its footprint still counts.

**Unaccounted**:
GPU memory in use that belongs to no model the dog can name. It is reported, never counted.

### Who holds what

**Client**:
A named caller of the dog, with its own key, so the dog can say who holds what.
_Avoid_: user, consumer

**Lease**:
A client's claim on a model, or on a bare footprint, for work that is not one request, such as an eight-hour benchmark or a job running its own GPU code. It ends when released, when its holder stops renewing it, when a holder on the same host dies, or when the maintainer revokes it.
_Avoid_: lock, reservation

**Reclaimable lease**:
A lease that keeps its model loaded past its idle time but leaves it reclaimable, and ends when the model is evicted.

**Idle lease**:
A lease whose holder has neither used its model through the dog nor sent a progress note for a while. It is reported, and released only when it asked to be.

**Revoke**:
To end someone else's lease by hand, for a holder that is alive but forgotten.

**Held**:
A loaded model that a lease names without allowing reclaim. It is never evicted, whatever is waiting.

**Reclaimable**:
A loaded model that no held lease names. It stays loaded until a waiter needs its room.

**Evict**:
To unload a reclaimable model to make room for a waiter, after its in-flight requests finish. Once an eviction is committed it is final, and new requests for that model wait.
_Avoid_: preempt, kill

### Waiting

**Waiter**:
A request or lease that cannot be served yet, with the reason it waits.

**Interactive**:
A waiter served ahead of batch ones. Requests are interactive unless they say otherwise.

**Batch**:
A waiter served after interactive ones. Leases are batch unless they say otherwise.

**Grace period**:
How long a reclaimable model must go unused before a batch waiter may evict it.
