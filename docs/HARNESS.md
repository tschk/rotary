# Engine-owned harness pieces

Hosts (telekinesis, apollo) embed these; they do not reimplement them.

## Hashline

`rx4::hashline` — tagged file reads (`[path#TAG]` + `N:line`) and fail-closed
`PUT` / `CUT` / `MV` / `REM`. Stale tags, elided or unseen lines, and no-ops
error. Some model families get a sloppy parse fallback after strict parse fails.

`read` accepts `"hashline": true`. When set, `limit` is the max visible line
count; `head`/`tail` are derived from `limit` only so they cannot exceed it.
`offset` is ignored on hashline reads (it remains the start line for plain reads).

`hashline_edit` applies a script against the visibility recorded by the last
hashline `read` of that path whose tag still matches. Without a prior hashline
read — or if the tag does not match that read — every line is treated as unseen
and the edit fails closed. Elided lines from that read also fail closed.
A successful edit invalidates the stored visibility; read again before the next
script. Sequential ops are bounded against the *current* buffer (CUT then PUT
returns `OutOfRange` instead of panicking).

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
