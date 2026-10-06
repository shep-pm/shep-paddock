# Coexistence is footprint arithmetic, with exclusions as overrides

Each model, or each placement of one, declares the VRAM and RAM it uses, and the dog admits a model only when the declared totals fit the host. A model whose use grows into whatever is free, like Strata's expert cache, declares `all`. Where a measured pairing disagrees with the arithmetic, the model lists an exclusion: IQ3_S plus laya in RAM fits on paper but is never allowed. Admission uses declared figures, not live free memory, because a greedy cache makes live numbers meaningless.

## Considered options

- llama-swap's sets, listing every allowed combination. No measuring, but every new model means re-checking every set by hand, and placements turn into separate names.
- Footprints alone. One line per new model, but the only way to forbid IQ3_S plus laya would be to overstate IQ3_S's RAM.

## Consequences

- Footprints must be measured, and a model's figures hold only for the config they were measured at. Strata's RAM changes with its context length.
