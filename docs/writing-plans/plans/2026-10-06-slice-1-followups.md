# Slice 1 Follow-ups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close issues #3 to #9, the edge cases and cleanups the review of slice 1 left open, without changing what the dog does for a config that works today.

**Architecture:** No new parts. Each item is a guard, a test, a log line or a comment in a file slice 1 wrote. The issues are the item lists; this plan adds the ruling on every item that asks for a decision, and the test that pins each fix.

**Tech Stack:** as slice 1: Rust 2024, MSRV 1.88, tokio current-thread, hyper 1, reqwest 0.13, shep-client 0.12, proptest.

**Spec:** `docs/brainstorming/specs/2026-10-04-slice-1-design.md`, with `CONTEXT.md` and `docs/adr/0001` to `0003`. The issue bodies (`gh issue view <n>`) are the item lists. Their line numbers are against `43c1ded`; find each spot by the named function when lines have moved.

## Global Constraints

- MSRV 1.88, edition 2024. `cargo +1.88 check --all-targets --all-features --locked` must pass.
- `#![forbid(unsafe_code)]` holds. No unsafe anywhere.
- One cargo shape while iterating: `cargo test --locked`. One cargo command at a time.
- CI gates before each commit: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features --locked`.
- House style: invoke the `rust-house-style` skill before writing Rust. IR-1..IR-48 apply; IR-47 (comments say only what code cannot) and IR-48 (no file over 1000 lines, weigh a split past 500) are the ones these edits brush against.
- Tests: hand-rolled fakes, every await bounded, real time with short test-only timeouts where a socket is involved (paused clock plus real sockets flaked in slice 1).
- Vocabulary from `CONTEXT.md`. Committed text says "the maintainer", never a name, and uses repo-relative paths. No internal ruling ids in committed text.
- One commit per item, or per group of items that cannot pass apart (say which in the body). Conventional subject, body with the reasoning, ending `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. A commit closing the last item of an issue says `Closes #N` in its body.
- Behaviour for the maintainer's config must not change except where an item says so.

## Review Focus

1. A restored `state.json` naming a lease id that is also granted fresh: the first lease must survive and nothing may end silently. Test in Task 1.
2. A guard added to `unloaded` must not leave a model stuck in Unloading when the real unload event arrives. Test in Task 1.
3. Waiting for a sheep to come online must stay bounded by `load_timeout`, and a sheep that never comes online must fail the load, not hang. Test in Task 2.
4. Rejecting `..` in a prefixed path must not reject a query string or an encoded `%2e` that the backend expects literally: reject only path segments equal to `..` after percent-decoding. Test in Task 3.
5. Lease routes answering 404 for another client's lease must still answer 403 for nothing, and the owner's own calls must be unchanged. Test in Task 3.

---

## Rulings on the items that ask for a decision

Each ruling is binding for its task. Where the issue offers two fixes, the ruling names one.

#3
- `grant` on a live id: refuse. The book leaves the live lease untouched and emits nothing for the second grant. Test: grant id 7, grant id 7 again for another client, the view still shows the first holder and no `LeaseEnded` was emitted.
- `HolderDetached` on a heartbeat lease: ignored. Test: the view keeps `attached: true`.
- A Reserved model whose waiters all left: it drops its claim, goes back to Unloaded, and no load starts. Its committed evictions stand (a committed eviction is final). Test: reserve, cancel the only waiter, no `Load` action and the declared totals no longer count it.
- `reload_held` before the walk: keep the order. A held model's room is already claimed by its lease, so an interactive waiter could not have used it. Document that in one line at the call.
- `loaded` setting `last_used = now`: keep. A just-loaded model gets its grace from the load, which stops a load and evict loop. Document in one line.
- `unloaded` with no guard: act only when the model is Unloading or Evicting; in any other state, ignore it. Test: an `Unloaded` event while Loaded leaves it Loaded.
- Eviction candidates of in-flight models: sort by `used_at`. An interactive waiter evicting an in-flight model (drain, then unload) is by design; document it at the sort.
- A reload removing a Reserved model: evictions committed for it stand; clear `for_model` on those slots so nothing names a model that is gone. Test after a reload.
- Two configured models naming one ollama model on one backend: refuse at validation with an error naming both. Test in config tests.
- The cleanup after a timed-out load: retry its stop the way `unload` does. Test: the fake fails the first stop, the second is sent.

#4
- The ollama stand-in carries the key `/api/ps` was read with. Test with an authed fake.
- Stand-in names include the backend, so two ollama urls listing one unconfigured name make two stand-ins. Test.
- An ollama that does not answer at startup: its models are restored Unloaded and an entry goes into `errors` naming the backend. Test via status.
- `Discovered.unknown`: drop the field.
- A hung but running leased sheep counts Loaded until its lease ends: accepted. One comment at the restore.
- Startup probes run concurrently (`join_all`), so the worst case is one probe's time, not the sum. Test with two slow fakes under a bound.
- A sheep model with no `ready` waits for the sheep's `online` event, bounded by `load_timeout`. Test both paths.
- `load_sheep` for a non-sheep backend, and a `ready` with no url: return errors. Tests.
- The give-up guard becomes `!= Some(Loading)`. Test the `None` case.
- Terminal lease events are never dropped: send them on an unbounded channel. Test: fill the bounded channel, the terminal event still arrives.
- `on_sheep` keeping the last model: wanted (the spec's "last started for"). One comment.
- The `.bad` rename failure test lives in Task 6.

#5
- `Bearer` matches case-insensitively. Test `bearer` and `BEARER`.
- A client reset logs at debug, not as "ended badly".
- Headers named in a client's `Connection` value are dropped. Test.
- A prefixed path with a `..` segment (after percent-decoding) is `400 bad_path`. Test `..`, `%2e%2e`, and that `a..b` and a query with `..` pass.
- Credential headers: `cookie` is dropped like `authorization`. Document in `SECURITY.md`.
- A sheep model's `name` rewrites `model` like an ollama model's. Test.
- Heartbeat `ttl` is capped at 1 hour: above that is `400 bad_ttl`. Test.
- A lease `note` is capped at 1024 bytes: above that is `400 note_too_long`. Test.
- Another client's lease answers `404`, as an unknown id does, so ids cannot be probed. `Held` naming the holder stays: naming who holds a model is the dog's point. Test both.
- A wrong method on a lease path is `405` with `Allow`. Test.

#6
- A repeated `--model`, `--note`, `--expected` or `--priority` flag is an error naming the flag. Test.
- `open()` gets a timeout like release. Test with a silent fake.
- The silence deadline survives wakeups. Test: wakeups that bring nothing do not push the deadline out.
- A signal that beats an in-flight `Granted` sends the release. Test.
- The pid reuse race: one comment at `forward`.

#7
- Duplicate client names or keys: refused at validation. Tests.
- Overlapping prefixes: refused at validation. Test `/v1` with `/v1/x`.
- `Client`'s `PartialEq` compares names only, written by hand. Test.
- `ConfigError` gains `Clone`, `PartialEq`, `Eq` where its sources allow (IR-19).
- `Listen`, `Size` and `Duration` errors keep echoing the value; their docs say these are never secret fields.
- The reported column counts characters. Test with a multibyte line.
- A typo inside an inline sheep table gets an error naming the field. Test.
- `Backend`'s `Debug` prints the args count. Exact-string test.
- `LoadError::Status` strips userinfo from the url. Test.
- A model's url is parsed once at config load; a bad one is a config error. Test.
- A changed `listen` on reload logs that it needs a restart. Test the log line through the watcher's result.

#8: every item as the issue states it. `proptest-regressions/` is committed, as proptest recommends.

#9: every item as the issue states it. The unreachable guards stay, each with a one-line comment saying what they defend.

---

### Task 1: Book and admission (#3)

**Files:** `src/book/lease.rs`, `src/book/mod.rs`, `src/book/admit.rs`, `src/book/wait.rs`, `src/book/backend.rs`, `src/book/reload.rs`, `src/config/mod.rs`, `src/engine/run.rs`, tests under `src/book/tests/`, `src/config/tests.rs`, `src/engine/run/tests.rs`.

- [ ] For each #3 item: write the test named in its ruling, run it and see it fail, fix, run it and see it pass, commit.
- [ ] Run `cargo test --locked` and the gates. The proptest in `src/book/tests/invariants.rs` must still pass.

### Task 2: Discovery and engine (#4)

**Files:** `src/discover.rs`, `src/backend/sheep.rs`, `src/backend/probe.rs`, `src/engine/state/*.rs`, `src/engine/mod.rs`, their tests.

- [ ] For each #4 item: test, fail, fix, pass, commit.
- [ ] The integration tests (`SHEP_BIN=$(command -v shep) CARGO_TARGET_DIR=target/integration cargo test --features integration --locked`) still pass, since the online wait reaches a real shepherd.

### Task 3: HTTP (#5)

**Files:** `src/http/mod.rs`, `src/http/proxy.rs`, `src/http/lease.rs`, `src/http/reply.rs`, `SECURITY.md`, their tests.

- [ ] For each #5 item: test, fail, fix, pass, commit.

### Task 4: CLI (#6)

**Files:** `src/cli/mod.rs`, `src/cli/run.rs`, their tests.

- [ ] For each #6 item: test, fail, fix, pass, commit.

### Task 5: Config (#7)

**Files:** `src/config/*.rs`, `src/backend/mod.rs`, `src/http/proxy.rs` (`target`), `src/config_watch.rs`, their tests.

- [ ] For each #7 item: test, fail, fix, pass, commit.

### Task 6: Tests (#8)

**Files:** as the issue names.

- [ ] Each #8 item as its own commit. A new test that fails exposes a bug: fix it in the same commit and say so in the body.

### Task 7: Docs and cleanups (#9)

**Files:** as the issue names.

- [ ] Each #9 item, batched into one commit per file where the items are comment-only.
