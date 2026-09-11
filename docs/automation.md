> Moved from `clean-language-foundation` — `02 components/hosts/clean-server/automation.md`, at foundation commit c72d093 (2026-09-11). Informative: the law it refers to lives in foundation, cited by id; nothing here binds another implementation (SDD-02).

# clean-server — Automation

This document tells the person setting up CI/CD for `clean-server` exactly which coverage tier, which nightly deep-test jobs, and which release-asset waits to configure. It is the placeholder-values checklist that names the exact values the bootstrap PR fills into the reference workflow templates so the three automation ADRs are satisfied on day one. Nothing here is a decision — decisions live in the ADRs and in `01-server.md`.

---

## Tier assignment

`clean-server` sits across two tiers because its blast radius differs by subsystem:

- **Tier 1 — bridge layer** (`bridges/`, WIT-typed entry points, marshalling). A bug here silently corrupts every plugin call across every hosted component.
- **Tier 2 — non-bridge layer** (HTTP framing, config loading, process lifecycle, `/healthz`). A bug here breaks the server but users can pin an older version to recover.

Per the coverage policy, the tier target enforced by `coverage.yml` is the **stricter of the two: Tier 1** (80% line / 75% branch / 60% mutation, MC/DC on named critical modules). Rationale: it is not practical to run two Coverage workflows against slices of the same crate; picking the higher bar means non-bridge code is over-covered relative to its tier, which is a cost the ecosystem accepts to protect the bridge layer without splitting the repo.

## coverage.yml — placeholder values

Copy `../../../scripts/reference-workflows/coverage.yml` into `.github/workflows/coverage.yml` and set:

```yaml
env:
  TIER: '1'
  TIER_TARGET_LINE: '80'
  TIER_TARGET_BRANCH: '75'
  BASELINE_LINE: '<measured on first run>'
  BASELINE_BRANCH: '<measured on first run>'
  BASELINE_MEASURED_ON: '<YYYY-MM-DD of first successful run>'
  MCDC_MODULES: 'src/bridges/,src/marshal/,src/wit_binding/'
```

- **`MCDC_MODULES`** — every module that translates between the host process and a guest component. If the actual crate layout at bootstrap differs, adjust paths but keep the semantic: **everything on the guest boundary** must have MC/DC.
- **Baselines** — set on first successful run of the workflow, not now. `BASELINE_LINE` and `BASELINE_BRANCH` are then only ever bumped upward per the ratchet rule.

## nightly-deep-tests.yml — job selection

Copy `../../../scripts/reference-workflows/nightly-deep-tests.yml` into `.github/workflows/nightly-deep-tests.yml` and enable the following jobs (flip `if: false` → `if: true`):

| Job | Enable? | Notes |
|---|---|---|
| `bridge-fuzz` | **yes** | Highest ROI job for this component. Fuzz targets: every WIT-typed entry point across every registered bridge. 300s per binding × parallel matrix over WIT worlds. Failures open `report_error` fingerprints keyed on `clean-server + bridge-fuzz + <binding> + <normalized-trap>`. |
| `config-matrix-boot` | **yes** | Matrix axes: `tls: [on, off]`, `auth: [none, basic, oauth]`, `storage: [memory, sqlite, postgres]`, `log_level: [info, debug]`. Trim to actual supported permutations if `01-server.md` declares a smaller matrix at bootstrap. |
| `leak-check` | **yes** | 10-min sustained traffic against `/healthz` + one representative bridge endpoint (`data.query` if available). Assert RSS growth < 50 MB and open-FD delta ≤ 10. |
| `plugin-load-determinism` | no | Framework's job, not server's — `clean-server` loads composed WASM, does not compose plugins. |
| `plugin-abi-compat` | no | Same reason. |
| `install-uninstall-matrix` | no | Manager's job. |
| `cross-platform-wasm-execution` | no | CLI's job. |

Additional server-specific job to add on top of the reference skeleton:

| Job | Purpose |
|---|---|
| `json-conformance-stress` | JSONTestSuite × 100 randomized transformations per input, run through the JSON bridge. Runtime budget ~15 min. Baseline: same pass/fail semantics as CI conformance job. |

Total nightly wall-clock: ~75 min. Fits the 90-min per-component budget in ADR-0029.

## wait-for-release-assets — consumers

Every workflow in `clean-server` that runs `cln install` MUST embed the step from `../../../scripts/reference-workflows/wait-for-release-assets.yml` before the install. Workflows that consume it:

| Workflow | Waits on |
|---|---|
| `nightly-deep-tests.yml` (all jobs that install `cln`) | `clean-compiler` |
| `nightly-canaries.yml` (see below) | `clean-compiler` and `clean-framework` |
| any future reporter-artifact replay workflow | `clean-compiler` |

`EXPECTED_TARGETS` values are the ones listed in the wait-for-assets reference file's inventory table — do not recompute.

## Release matrix (informative)

Not owned by this doc — set in `01-server.md` / implementation-notes. Recorded here so `nightly-deep-tests.yml` matrix jobs know what targets are shipped:

- `linux-x64` on `ubuntu-latest`
- `linux-arm64` on `ubuntu-24.04-arm`
- `macos-x64` on `macos-15`
- `macos-arm64` on `macos-14`
- `windows-x64` on `windows-latest`

If the matrix changes, this doc AND every consumer's `EXPECTED_TARGETS` value update in the same PR.

## Nightly canaries — trigger and content

Per ADR-0028, triggers are `schedule` + `dispatch` + `push` on `v*` — NOT `workflow_dispatch`-only. The wait-for-assets step is what makes the schedule safe.

Content: run the framework canary corpus against the freshly installed `clean-server` binary. Same corpus that the old project's `clean-server` canary used, unchanged in shape.

## Exceptions and deviations

None at bootstrap. Any deviation from this doc at bootstrap MUST be recorded here with a citation to the PR that introduced it.

## Metadata

- **Status:** Draft
- **Audience:** Person setting up `clean-server` CI/CD; reviewers of the bootstrap PR
- **References:** 07 — Component Bootstrap Checklist, Component spec, the coverage policy, ADR-0028, ADR-0029
