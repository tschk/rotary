# Ponytail audit — rx4 0.3.19

yagni: host-API islands rarely used in agent loop (multiagent, model_router, ranking, extract, repomap, rollout, routing, cost). Feature-gate or host-only reexport. [src/multiagent.rs src/model_router.rs src/ranking.rs src/extract.rs src/repomap.rs src/rollout.rs src/routing.rs src/cost.rs]
delete: marketplace + plugin surface if hosts do not ship store. [src/marketplace.rs src/plugin.rs]
yagni: dual Approver + Authorizer can collapse to AsyncAuthorizer only for new hosts (keep sync shim one release). [src/permissions.rs]
shrink: skills stack (engine+registry+curator+background_review+embeddings) ~3k lines optional forever — keep feature-gated, drop default mental load. [src/skill_* src/background_review.rs src/embeddings.rs]
shrink: graph_memory + dream_scheduler same story. [src/graph_memory src/dream_scheduler.rs]
stdlib: replace dashmap OnceLock todo store with parking_lot Mutex<HashMap> or tokio Mutex — one less concurrent map dep if only todo uses it. [src/tools/extended.rs]
yagni: moka tool cache — std HashMap+Mutex or quick_cache enough at tool-call rates. [Cargo.toml agent tool_cache]
delete: criterion/plotters stack from default mental SBOM if benches unused in CI. [dev-deps]
shrink: bin/rotary product CLI grows toward host (telekinesis) — keep thin. [src/bin/rotary.rs]
native: seatbelt/bwrap already right layer; do not add tree-sitter for shell. (keep)

net: ~-3500 lines if host-API islands + marketplace trimmed or feature-gated out of default docs; ~-2 deps if dashmap/moka dropped after microbench.
