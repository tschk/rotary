# Historical patch evaluation

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
  actual Epoch image digest; each shard records Python/package versions,
  6 GiB container memory limit, 1,024 PID limit and disabled networking.
- [Durable evidence archive](evidence.tar.gz): 2,500 unique cell records
  (500 reference plus 2,000 model cells), 2,448 grading evidence files,
  per-shard provenance and package lists. Every recorded grading evidence
  hash was independently checked. Archive SHA-256:
  `f602aaa0adfee37ea977c49a0128026d375d0cc985d4cf51c518f5ca94de708b`.
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
