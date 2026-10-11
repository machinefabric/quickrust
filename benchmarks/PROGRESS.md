# Progress

One line per recorded speed-index run (`haul-bench run --full --record`). Indices are
wall-time geometric means; 1.000 is parity with Cargo, above 1 is faster.

| Date | Haul | Change | haul | cargo-tuned | Notes |
|---|---|---|---|---|---|
| 2026-10-12 | cd9cb84 | Baseline: Haul observes and delegates; nothing optimized yet | 0.988 (cpu 0.986) | 1.069 (cpu 1.168) | Haul costs ~1–3% over Cargo, most on short scenarios (edit-leaf 0.84–0.99, no-op 0.89–0.99). Linux x86_64, 16 CPUs, rustc 1.98.1. |
