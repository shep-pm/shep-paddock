# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.3] - 2026-11-08


## [0.1.2] - 2026-11-08

### Fixed

- The silence-deadline signals test waits for its lease before it interrupts


## [0.1.1] - 2026-11-05

### Added

- Paddock shows the host and its models in a lookout pane

### Fixed

- An older snapshot never replaces a newer one
- Refuse a client named as the pane's operator


## [0.1.0] - 2026-11-05

### Added

- Add the host arithmetic admission rests on
- Read and validate the paddock section
- Admit models by footprint and evict reclaimable ones
- Hold models for leases, order waiters and refuse with a reason
- Reload config, restore leases and report a snapshot
- Load and unload models on sheep and on ollama
- Run the book's decisions against the backends
- List the flock on each subscription to find missed crashes
- Serve the endpoint with per-client keys
- Route by model and stream responses through
- Take, hold and release leases over HTTP
- Survive a dog restart with leases and loaded models intact
- Run a command under a lease and print the status
- Pass TERM and HUP on to the command and bound a silent stream
- Route ollama's own API by model
- List ollama-API models at GET /api/tags
- Read placements, and park a sheep's script
- Choose a placement when a model loads
- Let a reclaimable lease keep its model until a waiter needs the room
- Track lease activity and release an idle lease that asked to be
- Count strays the engine reports and forget the ones that go away
- Keep placements, strays and lease activity across a restart
- A one-model sheep is that model, at its saved placement
- Hear notes, and end reclaimable and idle leases
- Count a sheep started by hand as a stray
- Read nvidia-smi and measure each model's memory
- Survey the host every 30s for drift and unaccounted memory
- Find and forget strays in each survey
- Take reclaimable and idle-releasing leases, and hear notes **(BREAKING)**
- Take reclaimable and idle-releasing leases, and send notes
- A per-model sequences limit, so leases past it wait their turn
- Admit a bare lease against the host's totals, as a load is **(BREAKING)**
- Revoke a lease, keeping a bare job's memory counted while it may run
- Mark admin clients, and name a sheep model's container
- Save bare leases with their footprint and pid, as version 3
- Take a bare lease, keeping its pid only over loopback
- Revoke any lease as an admin client, over POST /paddock/leases/{id}/revoke
- Measure a bare job through its pid, and a model through its podman container
- Count a container running without its model as a stray, and stop it after its sheep
- Show each lease's footprint, measured VRAM, drift and revoke in the status
- Run a command under a bare lease, and stop it if the lease is revoked
- Revoke a lease, and show a bare lease's footprint, measured VRAM and revoke in the status
- Read the client key from shep's secret store when PADDOCK_KEY is unset
- Queue requests for a model with sequences by turn **(BREAKING)**
- Count a model's turns in its footprint through turn_ram
- Tell a sheep its slots and turn RAM before it starts
- Refuse PADDOCK_ env keys and sheep whose models disagree on turn_ram
- Show each model's footprint, with vram = all as all
- Report each model's measured_peak, turn_ram_bytes and turns
- X-Paddock-Lease names the turn a request takes
- Reload a loaded model once its changed launch settings can apply
- Say which lease a waiting request is under
- Pass SIGQUIT on to a bare command
- A waiting bare lease shows what it asks for
- Ask for the shepherd channel and stop on the shepherd's message
- Say when the dog rejoins the shepherd after a reload
- Report each model's loaded state, leases and waiters to the metrics dog
- Declare metrics = "strata" on a sheep model
- Report a loaded Strata model's totals, live state and GPU readings
- Read the dog's section with its secret references resolved
- Read the host's power from a Kasa plug or the GPU and CPU counters
- Add a Grafana dashboard for the paddock's series
- Read usd_per_kwh and eia_key_env from [meter]
- Keep the host's electricity price, set or looked up from where it is
- Show the host's electricity price in the status and the metrics
- A released lease says the energy it used and what it cost
- Declare a verb for shep's help

### Changed

- Stream process events as paddock's own type
- Pick ollama models once in the shared-model check
- Drop the unknown-sheep list only tests read
- Move the cross-model checks to their own module
- Make an attach stream's model an Option all the way down
- Drop Session.alive, the sender's weak handle says it
- Hold Live's client in an Rc
- Narrow Engine's clock, book and watchers to pub(super)
- Move value parsing and url redaction into values.rs
- Move the fake shepherd into a file of its own
- Move the property test's operations into a file of their own
- Count a model's requests in flight from the per-client map alone
- Drop start_load's fallback to a recorded backend
- Share the reclaimable lease ask between the book tests
- Share the reload-after-a-crash predicate
- Build the version error's list from READS
- Rename restored to as_found
- Move answering commands out of the state module
- Seed a stray only when the book will count it
- Split the survey tests by what they test
- Expect the survey's dead code instead of allowing it
- Move the bus event mapping into its own module
- Split the integration tier's harness out of its tests
- Split the survey's race tests out of its stray tests
- Move the reply tests beside reply.rs
- Move the lease stream reader out of run.rs
- Give turns a module of their own
- Move the take body's parsing beside the lease routes
- Give a model's building a module of its own
- Move the probe's tests to a file of their own
- Make room for more command-line and saved-state test files
- Give events and actions a module of their own
- Give run's stream watch a module of its own
- Give the property test's lease record a file of its own
- Give the lease view a module of its own
- Move Link and the key lookup into cli/link.rs
- Split the config tests into files by subject

### Fixed

- Keep secrets out of parse errors
- Gate every load on memory held now
- Reload held models after a crash and keep grace to request traffic
- Stop reloading a held model once its reload fails twice
- End leases by event time and treat in-flight as use
- Admit against the larger of loaded and configured figures
- Stop abandoned loads, end lease streams, clear stale marks
- Time out slow clients and back off on accept errors
- Time out slow request bodies and refuse unknown priorities
- Never follow redirects or proxies, and keep client keys off backends
- Route only whole segments under /paddock/leases
- Count every model ollama lists, configured or not
- Count sheep waiting to restart and keep a bad state file
- Leave the queue on a signal and catch stop signals first
- Share one subscription between process events and config changes
- End the process stream on a lag and tighten the new tests
- Never panic locating a TOML error
- Refuse at once behind a lease past its expected end
- Keep a crashed held model's room while it unloads
- Restore a leased sheep as its model when ready fails
- Never load two models on one sheep together
- Run a sheep's stops and starts in order
- Restore a leased ollama model as itself when ready fails
- Keep a removed model's sheep from a new model while it holds it
- Run a skipped quiet stop when the load that replaced it fails
- Print the host totals and the failed loads in paddock status
- Drop trailing slashes from backend and model urls
- Bound each unload attempt so a silent backend cannot hold the room
- Drop num_ctx from requests forwarded to ollama
- Refuse ollama's unload requests instead of loading
- Refuse a grant on a lease id that is already live
- Ignore a holder detaching from a heartbeat lease
- Drop a reserved model's claim once nothing waits for it
- Ignore an Unloaded event for a model not being unloaded
- Order eviction candidates by use that counts requests in flight
- Stop evictions naming a model a reload removed
- Refuse two models naming one ollama model on one server
- Retry the stop that cleans up after a timed-out load
- Drop a claim whose last waiter was refused in the same walk
- Say in the log when a timed-out load's stop is what fails
- Unload an ollama stand-in with the key that found it
- Name each ollama stand-in for its backend
- Report an ollama that does not answer at start
- Refuse a sheep load of a non-sheep model or a url-less check
- Wait for a sheep to come online when its model has no ready check
- Stop a load the book forgot, not only one it gave up on
- Never drop the event that ends a lease's wait
- Ignore an online from the sheep's process before the restart
- Fail a sheep load at once when the flock shows it stopped
- Refuse a repeated run flag
- Time out a take that the dog never answers
- Keep the silence deadline across wakeups
- Release a lease whose grant a signal overtook
- Match the Bearer scheme in any case
- Log a client's hang-up as a debug line
- Drop the headers a client's Connection names
- Refuse a prefixed path with a .. segment
- Keep a client's cookies from the backends
- Rewrite model to a sheep model's name
- Cap a heartbeat lease's ttl at an hour
- Cap a lease's note at 1024 bytes
- Answer 404 for another client's lease
- Answer 405 with Allow for a wrong method on a lease path
- Count encoded slashes as separators in the .. check
- Answer 404 for a lease path with an empty segment
- Refuse two clients with one name or one key
- Refuse a prefix that lies under another model's
- Compare clients by name, never by key
- Derive Clone, PartialEq and Eq on ConfigError
- Count the reported column in characters
- Name the field when an inline sheep table has a typo
- Print the argument count in Backend's Debug, not the arguments
- Strip userinfo from the url a LoadError reports
- Parse a model's url once at load and refuse one that is not a url
- Say a changed listen needs a restart
- Leave the url's password out of the shared ollama model error
- Cap ttl only on a heartbeat lease
- Leave url userinfo out of the config's Debug output
- Refuse a model url that is not http or https
- Walk the waiters again when a refusal drops a claim
- Say an ollama could not be asked at start, not that it was silent
- Strip userinfo from a url with no scheme
- Leave a url's query and fragment out of what the dog shows
- Name a waiter's softest block across placements
- Prefer a placement whose room is already coming
- Tell a waiter nothing new when only a lease's idle changed
- Keep a lease whose holder's request is queued from going idle
- End a lease restored past its idle end before it loads its model
- Keep a loaded model's old sheep excluded after a reload moves it
- Record the sheep a failed load's retry runs on
- Claim room again for every Reserved model when a stray turns up
- Ignore a stray on a process the book already counts
- Let a stray settle the retry a failed load was owed
- Save a holder's request within a minute
- Restore a lease in use at the restart as used then
- Save whenever the models holding memory change
- End a restored reclaimable lease whose model is gone
- Clamp restored activity between grant and restart
- Save what a config reload changes in state.json
- Keep a recorded sheep the dog's when it is not ready
- Count ollama models beside a version 1 file as the dog's
- Restore a placement only through the sheep's record
- Leave an ollama model unmeasured when its blob is unlisted
- Read a GPU process's arguments off the engine's thread
- Leave unaccounted unknown while a memory source went unread
- Keep a drift flag through a figure the survey did not read
- Give a survey reading a Debug that prints no url or args
- Skip a pid whose earlier /proc read is still stuck
- Stop waiting on a stuck blocking read when the dog exits
- Count a GPU process with unread args as unknown, not gone
- Forget a model's drift when one of its jobs reports
- Make dropping a survey's log lines a build failure
- Ignore a model ollama still lists after the dog unloads it
- Renew a lease on a PUT of {} as slice 1 did
- Round a lease's release_if_idle up to whole seconds
- Escape bidirectional controls in text the CLI prints
- Percent-encode the lease id in the note path
- Count a moved model on its old sheep at a restart
- Run one nvidia-smi at a time, so stuck ones never pile up
- Log once when a stalled survey stops surveys and resumes
- Drop a survey still waiting after two periods
- Clear the nvidia-smi flag even when a query panics
- Count a moved model on an old sheep given a new model
- Keep the saved stray flag on an unready ollama model
- Drop a lease take whose asker has already hung up
- Escape the Arabic letter mark with the other bidi controls
- Leave an ollama model unmeasured when its blob runs twice
- Redact the userinfo of a url that starts with //
- Reread a blob every survey when /api/ps gives no digest
- Free a pid's read slot even when the read panics
- Keep a process's arguments out of its Debug
- Drop a sheep's record once nothing the dog loaded runs there
- End only the stopped sheep's record, and save a quiet stop's
- Read a container's cgroup only below the cgroup root
- Leave a container unread when its cgroup or memory cannot be read
- Count a container removed between podman's two questions as stopped
- Say in the survey's docs that it measures bare leases and reads containers
- Count a container process that exited before its memory was read as 0
- Count a zombie, and a status read failing as its process exits, as 0
- Count a container still running after its stray's unload again
- Show a bare lease's idle time as null in the status
- Fail a bare waiter a reload leaves too big for the host
- Run a bare lease's command in its own process group
- Stop a bare run's command when its lease cannot be attached again
- Bound a bare run's attach by its reconnect time
- Hold a killed bare group's lease until the group is gone
- Leave a container unread when a cgroup in it cannot be read
- Refuse a heartbeat lease whose ttl is 0
- Name an unreachable dog by its redacted url
- Walk GPU processes' parents in every survey
- Say a container name must be ASCII
- Compare clients' admin and protected marks
- Say a bare lease takes --vram, --ram or both
- Say revoke cannot end another protected client's lease
- Skip a cgroup entry removed during the walk, and test as root
- Say to set either the variable or the secret when no key is found
- Hold an ended lease's turn until its request finishes
- Poll the section every 30 s so an outside dogs.toml edit applies
- Check placement env keys for PADDOCK_ before the placements are judged
- Start a model's peak again whenever it stops holding memory
- Leave a touched model's peak alone when a survey began before the touch
- Requests naming one lease take its turn first come
- End a reloading model's reclaimable leases when the config drops it
- Say the exact --grace in the kill message
- Name a drifting bare lease as every other line does
- A bare job's unreadable GPU process is the job's own
- Reset a model's failure log when it stops being scraped
- Zero a Strata gauge that a later scrape leaves out
- Declare the permissions the dog uses in its manifest
- Make a second nvidia-smi wait for the running one rather than skip
- Read ipinfo's spelling of the District, Washington, D.C.
- Name the power series' meter in a label the metrics dog accepts
- Leave the dollars out when part of a lease's energy had no price

### Performance

- Ask every backend at once at start
- Mark only a request for the leased model as activity
- Save an end of use at once only if the file shows it
- Check a feed's saves without building a map or set
- Ask whether a sheep is busy only for an online event

