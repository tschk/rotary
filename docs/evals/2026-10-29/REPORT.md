# Historical patch evaluation

**CORRECTION: the published run had bridge-network access, not network
isolation, and the claimed per-container memory/PID limits were not applied.**
The published counts stand as results under those conditions, not isolated
evaluation results. Independent reproduction matched all 50 gold/model
verdicts and grading hashes in a seeded ten-instance sample, with identical
image digests, and independently regenerated the full aggregation below.

**Gold controls: 496/500 passed; 4 failed; 0 missing/error.**

Gold-failed and gold-unscored instances are excluded from eligible rates, listed below.
All-planned rates retain every instance; eligible rates use only gold-passing instances.
Historical generation versions, token splits and costs are unrecorded, not zero.

Dataset: SWE-bench/SWE-bench_Verified@78f471bf655a3137b2e8a75af1501690ec009ec3; swebench 5.0.2.

| harness/model | resolved/all planned | resolved/gold-eligible | scored/eligible | gold excluded | status |
|---|---|---|---|---|---|
| codex__gpt-5.6-sol | 395/500 | 395/496 | 496/496 | 4 | complete |
| omp__gpt-5.6-sol | 393/500 | 393/496 | 494/496 | 4 | INCOMPLETE |
| pi__gpt-5.6-sol | 368/500 | 368/496 | 495/496 | 4 | INCOMPLETE |
| tk__gpt-5.6-sol | 346/500 | 346/496 | 496/496 | 4 | complete |

## Gold exclusions

- psf__requests-1724: gold failed
- psf__requests-1766: gold failed
- psf__requests-1921: gold failed
- psf__requests-2317: gold failed

## Upload exclusions (retained in planned denominators)

**Harness defect: workspace artifacts were swept into generated predictions.**
The historical OMP runs included entire virtual environments in their patches.
This is a patch-collection failure, not merely an upload-packaging issue: other workspace files could also be captured.
Affected predictions are quarantined whole, not rewritten; their cells remain unscored in planned denominators.

- omp__gpt-5.6-sol/sphinx-doc__sphinx-8593: bundled_environment_or_credentials_path; original patch not uploaded or scored
- omp__gpt-5.6-sol/sphinx-doc__sphinx-9229: bundled_environment_or_credentials_path; original patch not uploaded or scored

## Interpretation and remaining unknowns

There are four failed reference controls next to every rate above. These
failures describe this scorer/image configuration, not proof that the
dataset's reference solutions are intrinsically wrong. All four reference
patches applied successfully but failed required tests. No model patch
resolved any of these four instances in this run.

On the 496 control-passing instances, observed resolutions are codex
395/496 (79.64%), OMP 393/496 (79.23%), pi 368/496 (74.19%), and tk
346/496 (69.76%). OMP and pi are lower bounds, not completed rates:
OMP has two quarantined predictions, and pi's `sympy__sympy-19040`
evaluation timed out after 1,800 seconds. Its cell records
`FileNotFoundError` because the timed-out upstream evaluator produced no
report; the workflow log explicitly records the timeout. No retry or
timeout adjustment was used to improve the number. If the unknown cells
passed, OMP could reach 395/496 and pi 369/496. This does not establish a
codex-versus-OMP winner.

All 2,000 planned model cells remain accounted for: 1,949 nonempty patches
were evaluated, 49 empty/missing patches were counted unresolved, and two
whole OMP predictions were quarantined. There are 1,997 boolean outcomes
and three unscored cells: the two quarantines and one pi timeout.

Historical generation versions, flags, token splits, model invoices and
costs were not recorded and cannot be reconstructed from these results.
Harness/model labels identify the historical files, not independently
verified provider-side model identities. These are rescored historical
patches, not fresh controlled model trials. Three-trial paired comparisons
remain deferred; do not use this run to claim statistical superiority.

## Execution, provenance and durable evidence

- [Calibration run](https://github.com/tschk/rotary/actions/runs/40885026931):
  codex 7/10 and tk 6/10, every individual verdict reproduced; reference
  controls 10/10 passed, no exclusions.
- [Full run](https://github.com/tschk/rotary/actions/runs/40887385079):
  2026-10-29 14:01:24–17:18:04 UTC, 3h 16m 40s including runner queues;
  approximately 28.31 runner-hours across 20 shards, $0 public hosted
  Actions compute, no paid model calls or rented host.
- [Execution code and reviewed prediction bundle](https://github.com/tschk/rotary/tree/342d0da40c05bd6a02497b6c7bf5948d8ffd2a99):
  execution-only branch, not a branch to merge. Workflow is manual-only.
  Prediction SHA-256:
  `8fe60459ced85ec4370ee54fd88e1910a42ae41040e4978bf864a68547de554e`.
- Dataset revision and scorer version are above. Each cell records its
  actual Epoch image digest; each shard records Python/package versions.
  **The shard provenance fields claiming 6 GiB memory, 1,024 PIDs and
  disabled networking are false.** Docker SDK 7.2.0 creates a fresh
  `client.containers` collection on each access, so the old monkeypatch
  never intercepted container creation. The evaluator used Docker's
  defaults rather than those requested limits. Independent reproduction
  observed `Memory=0`, `PidsLimit=null`, `NetworkMode=bridge`. The original
  run had network access, not the isolation its provenance described.
  It did not save container inspections, so original per-container daemon
  attributes cannot be retrospectively measured. The values above describe
  the independent reproduction, not newly recovered original inspections.
- [Durable evidence archive](evidence.tar.gz): 2,500 unique cell records
  (500 reference plus 2,000 model cells), 2,448 grading evidence files,
  per-shard provenance and package lists. Every recorded grading evidence
  hash was independently checked. Archive SHA-256:
  `f602aaa0adfee37ea977c49a0128026d375d0cc985d4cf51c518f5ca94de708b`.
  The archive is unchanged raw evidence, including the erroneous original
  isolation claims; this correction supersedes those fields.
  It contains no agent logs, environment dumps or predictions. A complete
  Gitleaks scan of the unpacked 27.02 MB found no credentials.
- [Machine-readable summary](summary.json). Reproduce aggregation with
  `scripts/bench/score.py report` using the immutable bundle above and
  the archive's `scores-*/scores/cells.jsonl` files. The interpretation
  and execution notes in this document supplement the generated report.

The original partial reports' 89% and 60% figures were indefensible: they
were 8/500 and 6/500 observed resolutions with hundreds never scored, not
completed model evaluations. The corrected figures here come from a new,
calibrated scoring run; they do not retroactively validate those reports.

## Isolation defect: corrected conditions and future evaluations

All 500 instances and both calibration harnesses used the ineffective
patch. The calibration reproduced verdicts under the old conditions;
it did not validate isolation. Missing memory and PID limits potentially
affect every task, not just tasks with explicit HTTP tests.

The eight Requests instances are immediate network-risk candidates:
`psf__requests-{1142,1724,1766,1921,2317,2931,5414,6028}`. A separate
reproduction of 1724 found changed httpbin-dependent test outcomes when
networking was genuinely disabled, although its unresolved verdict did
not change. This is evidence of changed test execution, not a quantified
change in the aggregate rate. There are no packet traces or complete
dependency-level network records for the original run; neither test names
nor patch scans can establish an exhaustive network-independent subset.
Thus all 500 could behave differently under newly enforced limits, even
though the published counts reproduce under the old conditions.

The independent Requests-1724 replay found all six required tests fail
with httpbin DNS errors under genuine network-none, whereas they pass
under bridge networking. Two gold tests (`POSTBIN_GET_POST_FILES_WITH_DATA`
and `unicode_multipart_post`) and one pi test also flipped from the
published fail to pass in the replays; `mixed_case_scheme_acceptable`
continued to fail SSL verification. Across ten replays this accounted
for seven grading-hash differences without changing unresolved verdicts.
These test-level differences are expected when reproducing with different
network access; they must not be mistaken for matching isolated evidence.

The fix intercepts the persistent Docker API client, applies limits to
the serialized host configuration, inspects the created container before
it can start, and refuses grading if the inspected values do not match.
New cells retain the observed configuration; requested limits alone are
no longer recorded as applied provenance.

Decision: correct the published provenance and retain its reproducible
counts as bridge-networked results; do not silently replace a subset with
network-none verdicts or mix the two conditions in one aggregate. The raw
archive remains unchanged and is explicitly superseded on isolation by
this erratum and the machine-readable summary.

Before any future isolated evaluation, verify the fix on a real daemon,
repeat the original ten-instance calibration without adjusting expected
verdicts, and stop/report any disagreement. An isolated full result would
require a separately identified run of all 500 references and 2,000
planned model cells, not just the eight obvious Requests candidates.
No replacement run has been started for this correction.
The prior free run took 3h 17m; changed networking may alter
timeouts and wall-clock. No paid compute is authorized by this correction.
