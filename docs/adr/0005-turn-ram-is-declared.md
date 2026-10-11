# A backend's conversation cache is sized from declared turn RAM

A model declares the RAM one turn holds, the dog counts `ram + (sequences + 1) × turn_ram` and tells the backend the slots and the RAM per turn, and the backend sizes its cache from them. The cache could instead take whatever RAM is free, but the dog's books would then stop saying what the host holds.

## Considered options

- Strata's own dynamic mode: a large cache budget with its free-RAM floor, left out of the dog's books. Strata checks free RAM only when it parks a conversation and never gives RAM back, so laya's RAM placement or a bare job admitted after the cache grew would run the host out of memory.
- The dog grants the cache whatever its books have free at load, and counts the grant. The books stay true, but anything that wants RAM later must evict the model to get it back, and the grant is fixed until the next load. Left as a follow-up if declared turn RAM proves too small.

## Consequences

- `turn_ram` is measured from the sessions a model actually serves. A conversation longer than it is not parked and reads its prompt again, and nothing else breaks.
