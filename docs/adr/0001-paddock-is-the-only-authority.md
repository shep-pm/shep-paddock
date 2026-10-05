# The paddock is the only thing that loads or unloads a model

Before the paddock, five things on the host decided what was loaded: Strata unloaded itself after two idle hours, ollama did the same through `OLLAMA_KEEP_ALIVE`, Strata's `before_load` hook unloaded ollama's models, the Strata launch script stopped laya before IQ3_S, and kelpie's `gpu` lock covered clients on one Mac. The dog adds up declared footprints to decide what fits, so any of those acting on their own makes its totals wrong. All of them go: Strata's `idle_unload_s` is off, ollama runs with `OLLAMA_KEEP_ALIVE=-1`, the hook and the script's laya handling are removed, and the dog strips `keep_alive` from requests it forwards to ollama.

ollama also moves from a system unit to a user sheep, so shep supervises its process and nothing on the host manages a model outside shep and the paddock. The dog still loads and unloads ollama's models through its API (`/api/generate` with `keep_alive` set to `-1` or `0`), since one ollama process serves several models.

## Consequences

- Re-enabling Strata's idle unload or ollama's keep-alive looks like a harmless tidy-up and is not. Idle unload is the dog's job.
- ollama's model store moves once, from the system location to one the user can read.
- Anything on the host can still call a backend's port directly. The dog does not block that, but it reports a model it did not load as a stray and counts its footprint.
