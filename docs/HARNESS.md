# Engine-owned harness pieces

Hosts (telekinesis, apollo) embed these; they do not reimplement them.

## Hashline

`rx4::hashline` — tagged file reads (`[path#TAG]` + `N:line`) and fail-closed
`PUT` / `CUT` / `MV` / `REM`. Stale tags, elided or unseen lines, and no-ops
error. Some model families get a sloppy parse fallback after strict parse fails.

`read` accepts `"hashline": true`. `hashline_edit` applies a script.

## Prewalk / plan-yolo

`rx4::prewalk::Prewalk` — investigate on the big model; the first real write
switches one-way to the smol/apply model.

- `RX4_PREWALK=1`
- `RX4_SMOL_MODEL`
- `RX4_INVESTIGATE_MODEL`

## AVO

`rx4::avo` — `P_t`, two-part `f` (incorrect ⇒ 0), commit-if-better, stall
detect. `scripts/avo-commit-if-better.sh` refuses `main`/`master` and never
pushes.
