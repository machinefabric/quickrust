# Progress

One line per recorded speed-index run (`qrust-bench run --full --record`). Indices are
wall-time geometric means; 1.000 is parity with Cargo, above 1 is faster.

| Date | QuickRust | Change | qrust | cargo-tuned | Notes |
|---|---|---|---|---|---|
| 2026-10-12 | cd9cb84 | Baseline: QuickRust observes and delegates; nothing optimized yet | 0.988 (cpu 0.986) | 1.069 (cpu 1.168) | QuickRust costs ~1–3% over Cargo, most on short scenarios (edit-leaf 0.84–0.99, no-op 0.89–0.99). Linux x86_64, 16 CPUs, rustc 1.98.1. |
| 2026-10-12 | 3863969 | Checkpoint 1: fast settings, artifact cache (dependencies + workspace units), rustup proxy skipped, parallel frontend on stable | 1.757 (cpu 1.424) | 1.072 (cpu 1.163) | qrust 2.27 · ripgrep 1.53 · axum 1.56. Regressions found: ripgrep edit-core 0.58 (parallel frontend on incremental units), rebuild-after-clean 3.7-3.8 on ripgrep/axum (small workspace units not stored). qrust-compat 0.987. |
