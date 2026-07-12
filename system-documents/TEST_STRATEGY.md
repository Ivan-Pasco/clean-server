# Clean Server — Test Strategy

**Owner:** clean-server component
**Version:** 1
**Last verified:** 2026-07-12

This document defines the tiered test strategy for `clean-server`, which hooks it fires on, and the policies that keep the strategy from decaying.

It is the authoritative reference. Any change to CI, git hooks, or test tiering must land in the same commit as an update to this document. `CLAUDE.md` links here — it does not duplicate the content.

---

## 1. Design principles

1. **Tiered by cost.** Fast checks run on every save/commit; expensive checks run before push; the most expensive checks run in CI and nightly. A single all-or-nothing test tier would either be too slow to run locally or too shallow to catch regressions.
2. **Same test binary, different filters.** Every tier runs `cargo test` with a specific set of `--test` / `--lib` / `--skip` filters. No test lives in only one tier by accident — the filter is the only difference.
3. **No test placeholders.** A test file that exists but asserts nothing is worse than no file, because it lies about coverage. The policy scanner (§ 5) rejects this.
4. **The strategy is code, not intent.** The rules in § 5 are enforced by `scripts/check_test_policy.sh`, not by reviewer discipline. If the check is green, the strategy is green.
5. **Hooks are opt-in but the rules are not.** Developers install hooks via `scripts/install_hooks.sh`. Whether they run locally or not, CI enforces the same policies on push.

---

## 2. Test tiers

| Tier | Trigger | Target time | What runs | Purpose |
|------|---------|-------------|-----------|---------|
| **T1 · Fast** | `pre-commit` git hook + `make check-fast` | < 30 s | `cargo fmt --check`, `cargo clippy --lib -- -D warnings`, `cargo test --lib`, placeholder scan (§ 5) | Catch obvious mistakes before commit |
| **T2 · Medium** | `pre-push` git hook + `make check-medium` | < 3 min | T1 + all bridge/registry/plugin-stub/wasm-alignment integration tests | Catch bridge-signature drift, plugin-stub drift, WASM alignment regressions before code leaves the machine |
| **T3 · Full** | CI on PR / `master` push + `make check-full` | < 15 min | T2 + `host_functions_test.rs` (real cln → wasm → server E2E) + `test_spec_compliance` + `test_layer3_spec_compliance` + server smoke test | Prove that the release candidate can boot, serve HTTP, and satisfy every registry contract |
| **T4 · Deep** | Nightly workflow (`nightly-canaries.yml`) + `workflow_dispatch` | up to 30 min | Layer 2 canary matrix against latest compiler tag, cross-component canary report | Detect drift between compiler releases and this server, feed the L3 aggregator |

### Tier membership (source of truth)

The exact set of tests per tier is defined by `scripts/check_test_policy.sh` and by the `[test-tiers]` block near the top of `Makefile` / CI. If you add a new test file:

1. Assign it to a tier (T1 unit, T2 integration, T3 E2E, T4 canary).
2. Add its filename to the appropriate section of `scripts/check_test_policy.sh` (const `TIER1_FILES`, `TIER2_FILES`, `TIER3_FILES`).
3. The policy scanner will fail the build if a test file is present on disk but not registered in a tier.

This prevents the "orphan test file" failure mode where a test is written, forgotten, and never actually runs.

---

## 3. Hook layers

### 3.1 pre-commit (T1, local)

Runs on `git commit`. Blocks the commit on failure. Can be bypassed with `--no-verify` for emergencies only.

Runs:

```bash
scripts/check_test_policy.sh --tier 1
cargo fmt --check
cargo clippy --lib -- -D warnings
cargo test --lib --quiet
```

Duration target: < 30 seconds on a warm cache.

### 3.2 pre-push (T2, local)

Runs on `git push`. Blocks the push on failure.

Runs:

```bash
scripts/check_test_policy.sh --tier 2
cargo test --lib --quiet
cargo test --test bridge_contract_test
cargo test --test bridge_compliance_test
cargo test --test canvas_stubs_test
cargo test --test ui_stubs_test
cargo test --test wasm_alignment_test
cargo test --test jobs_persistence_test
cargo test --test jwt_refresh_rotation_test
cargo test --test reset_token_bridge_test
cargo test --test string_split_test
cargo test --test page_guard_redirect_test
cargo test --test jobs_bridge_test
```

Duration target: < 3 minutes.

### 3.3 CI on PR / push to master (T3, workflow)

Full T2 lane plus:

- `cargo test --lib -- test_layer3_spec_compliance` (currently skipped in CI — this strategy re-enables it)
- `cd host-bridge && cargo test --lib -- test_spec_compliance`
- `cargo test --test host_functions_test` (the E2E HTTP suite; requires a compiled `cln` binary — CI installs one, per compiler CI convention)
- `cargo test --test server_smoke_test` (see § 4)
- `scripts/check_test_policy.sh --tier 3`

Duration target: < 15 minutes.

### 3.4 Nightly canaries (T4, workflow)

Existing `.github/workflows/nightly-canaries.yml`. Unchanged by this strategy; documented here so it's not orphaned.

---

## 4. Server smoke test

`tests/server_smoke_test.rs` proves that the binary — not just the linker — can start, serve, and stop.

Rationale: every existing integration test uses either `create_linker` alone (no HTTP path) or the `host_functions_test.rs` harness (which requires a real `cln` compile on every test). Neither exercises the release binary's boot path in isolation. A boot regression that survives all current tests would ship.

The smoke test:

1. Spawns `target/{debug,release}/clean-server` with a tiny embedded no-op WASM (WAT-compiled at test time; no external compiler needed).
2. Waits for `GET /` to return anything (2xx/3xx/4xx all acceptable — we only care that HTTP is served, not a specific response).
3. Sends SIGTERM and asserts a clean shutdown within 2 seconds.

Deliberately small so it fits in T3 without adding minutes.

---

## 5. Placeholder / test-quality policy

Enforced by `scripts/check_test_policy.sh`. This is the mechanism that keeps the strategy from decaying.

The scanner rejects a commit or CI run if any of the following are true:

| # | Rule | Rationale |
|---|------|-----------|
| P1 | A file under `src/` or `host-bridge/src/` contains `todo!()`, `unimplemented!()`, or `panic!("not implemented")` — unless the exact `file:line` is in `scripts/.test-policy-allowlist` | Placeholders in production code hide unfinished work behind types |
| P2 | A file under `src/` or `host-bridge/src/` contains `// TODO` or `// FIXME` outside allowlist | Same, in comment form. Convert to `TASKS.md` entries or dashboard reports |
| P3 | A `#[test]` or `#[tokio::test]` function with zero `assert*!` / `panic!` / `expect(` / `.unwrap()` in its body | A "test" that cannot fail is a lie about coverage |
| P4 | A `#[ignore]` attribute on any test unless the same line has an `// allowlisted: <reason>` marker | Ignored tests silently rot; either fix them or delete them |
| P5 | A file matching `tests/*.rs` that is not listed in a tier in `scripts/check_test_policy.sh` | Orphan test files never run in CI, giving false confidence |
| P6 | A test file that contains zero `#[test]` / `#[tokio::test]` functions | Empty test file = placeholder |

### Allowlist format

`scripts/.test-policy-allowlist` is a small file with one entry per line:

```
# format: <rule> <file>:<line> <one-line reason>
P1 host-bridge/src/sys.rs:279 Windows PPID stub — non-portable API, tracked separately
```

Entries must be reviewed when the underlying file changes; the scanner warns (but does not fail) if an allowlisted line has moved.

### Running the scanner manually

```bash
scripts/check_test_policy.sh --tier 3     # full check, matches CI
scripts/check_test_policy.sh --tier 1     # quick check, matches pre-commit
scripts/check_test_policy.sh --explain    # print current tier assignments and allowlist
```

---

## 6. When to update this document

Update `TEST_STRATEGY.md` in the same commit as:

- Adding a new test file (§ 2 tier assignment)
- Adding a new git hook or CI job (§ 3)
- Adding a new allowlist entry (§ 5)
- Changing the policy rules P1–P6

`CLAUDE.md` for this component links to this document — do not duplicate policy text there.

---

## 7. Non-goals

- **Code coverage percentage.** The strategy targets categories of failure, not a coverage number. A high coverage % on trivial getters would be worse than the current targeted tests.
- **Property-based / fuzz testing.** May be added later as a new tier; not in scope for the initial strategy.
- **Load / performance tests.** These belong in a separate benchmarking effort; the smoke test in § 4 only checks that the server boots, not how fast it responds.
