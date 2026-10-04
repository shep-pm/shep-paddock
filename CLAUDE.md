# shep-paddock

A shep dog that leases one host's GPU and RAM to model servers and jobs, behind a single endpoint. MIT OR Apache-2.0.

It runs on the maintainer's GPU host as an adopted shep dog. Clients name a model at one endpoint, and the dog leases the GPU and RAM, starts and stops the sheep serving each model, and queues what does not fit. Nothing is built yet: start from `docs/handoff.md`.

## Commands

- `cargo test --locked` is the test shape.
- CI's gates, all required: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features --locked`, and `cargo +1.88 check --all-targets --all-features --locked` for the MSRV.
- One cargo command at a time: they share the target-dir lock.
- `rust-toolchain.toml` floats on stable, like CI. When CI's clippy flags something a local run did not, `rustup update stable` first.

## Where things live

- `docs/handoff.md`: the requirements, the measured consumers and their coexistence rules, the prior-art research, and the shep APIs to build on.
- The lease engine being extracted lives in shep-kelpie's `src/lease/`; the handoff names its files and docs.
- shep-log-rotate is the reference dog for the crate's shape.

## Style

- Invoke the `rust-house-style` skill before writing or reviewing Rust. The rules are shep-pm/rust-house-style, IR-1..IR-48. This repo's exceptions go in `docs/rust-house-style-addendum.md`, which wins where the two disagree.
- `#![forbid(unsafe_code)]` holds through `[lints]` in Cargo.toml.
- shep's vocabulary: a `sheep` is one managed process, the plural is `flock`, dogs are plugin processes, and the daemon is only ever "the shepherd".
- Committed text says "the maintainer", never a name, and uses repo-relative paths.

## Commits and pull requests

- Conventional subjects: `type(scope): summary`, with types `feat` `fix` `perf` `refactor` `docs` `test` `ci` `chore` `style`, and `!` on the commit that breaks something. `.githooks/commit-msg` checks locally (run `git config core.hooksPath .githooks` once per clone) and `.github/workflows/commits.yml` checks every pull request.
- Pull request titles are conventional too. `merge_commit_title` is set to the PR title, so a merge commit's subject is the title.
- Merge with a merge commit, never a squash. Release pull requests are the exception, and their workflow squashes them itself.
- One commit per item. Bodies carry the full reasoning.
- release-plz writes CHANGELOG.md from commit subjects, so never hand-write an entry. A merged release pull request publishes to crates.io on its own.

## Agent skills

### Issue tracker

GitHub Issues on `shep-pm/shep-paddock`, via `gh`. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default roles, label string equal to role name. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` at the root and ADRs in `docs/adr/`, both created when a term or decision is first settled. See `docs/agents/domain.md`.
