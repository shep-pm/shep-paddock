# shep-paddock

[![crates.io](https://img.shields.io/crates/v/shep-paddock.svg)](https://crates.io/crates/shep-paddock)
[![License](https://img.shields.io/crates/l/shep-paddock.svg)](https://github.com/shep-pm/shep-paddock#license)

A shep dog that leases one host's GPU and RAM to model servers and jobs, behind a single endpoint.

A dog for [shep](https://github.com/shep-pm/shep) on a host where several model servers share one GPU. Clients name a model at one endpoint; the dog starts and stops the sheep that serve it, following rules about which models can run together, queues requests that do not fit yet, and lets long jobs hold a lease so nothing evicts them.

Not built yet. `docs/handoff.md` holds the requirements and research.

## License

MIT OR Apache-2.0, at your option.
