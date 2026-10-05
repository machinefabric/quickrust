//! Evidence-backed, ranked recommendations (FR-150).
//!
//! A recommendation exists only when the report contains the evidence for it.
//! Expected gains are labelled `measured`, `modeled` or `unknown`; a number is
//! shown only when it was measured or follows mechanically from measurements
//! (FR-150.2). Mutually exclusive strategies are one recommendation with
//! alternatives, never two simultaneous actions (FR-150.3).

use crate::util::{human_bytes, human_dur, vstr};
use crate::waste::Index;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn weight(level: &str, table: [(&str, f64); 3]) -> f64 {
    table.iter().find(|(l, _)| *l == level).map(|(_, w)| *w).unwrap_or(1.0)
}

/// Ranking: measured value first, discounted by confidence, risk and effort (FR-150.4).
fn score(rec: &Value) -> f64 {
    let impact = &rec["impact"];
    let seconds = impact["avoidable_time_ns"].as_u64().unwrap_or(0) as f64 / 1e9;
    let gib = impact["reclaimable_bytes"].as_u64().unwrap_or(0) as f64 / (1u64 << 30) as f64;
    let value = match impact["kind"].as_str() {
        Some("measured") => seconds + gib * 5.0,
        Some("modeled") => (seconds + gib * 5.0) * 0.5,
        _ => 0.05,
    };
    let confidence = weight(rec["confidence"].as_str().unwrap_or(""), [("high", 1.0), ("medium", 0.6), ("low", 0.3)]);
    let risk = weight(rec["risk"].as_str().unwrap_or(""), [("low", 1.0), ("medium", 2.0), ("high", 4.0)]);
    let effort = weight(rec["effort"].as_str().unwrap_or(""), [("low", 1.0), ("medium", 2.0), ("high", 4.0)]);
    value * confidence / (risk * effort)
}

struct Rec(Value);

#[allow(clippy::too_many_arguments)]
fn rec(id: String, rule: &str, category: &str, title: String, evidence: Vec<String>, confidence: &str, risk: &str, effort: &str, impact: Value, verification: &str, compatibility: &str, remediation: &str) -> Rec {
    Rec(json!({
        "id": id, "rule": rule, "category": category, "title": title, "status": "detected",
        "evidence": evidence, "impacted_scenarios": [], "confidence": confidence, "risk": risk, "effort": effort,
        "impact": impact, "verification": verification, "compatibility": compatibility, "remediation": remediation,
    }))
}

impl Rec {
    fn scenarios(mut self, s: &[&str]) -> Rec {
        self.0["impacted_scenarios"] = json!(s);
        self
    }
    fn with(mut self, key: &str, v: Value) -> Rec {
        self.0[key] = v;
        self
    }
}

fn unknown_impact(why: &str) -> Value {
    json!({ "kind": "unknown", "basis": why })
}

pub fn generate(report: &Value, index: &Index) -> Vec<Value> {
    let mut out: Vec<Rec> = Vec::new();
    let workload = report["policy"]["workload"].as_str().unwrap_or("local-dev");

    // Build scripts that rerun without declaring their inputs (FR-060.1).
    for s in report["build_scripts"].as_array().into_iter().flatten() {
        if s["state"] != json!("over-invalidating-candidate") {
            continue;
        }
        let runs = s["history"]["runs"].as_u64().unwrap_or(0);
        let per_run = s["duration_ns"].as_u64();
        out.push(
            rec(
                format!("rec:build-script-rerun:{}", vstr(s, "package")),
                "build_script_broad_invalidation",
                "build-script",
                format!("Build script of `{}` reruns on any package change", vstr(s, "package")),
                vec![
                    "the script's output contains no cargo::rerun-if-changed or cargo::rerun-if-env-changed directive".into(),
                    format!("it ran in {} recorded build(s), {} total", runs, human_dur(s["history"]["total_ns"].as_u64().unwrap_or(0))),
                    "without directives Cargo reruns the script whenever any file in the package changes, then recompiles the crate".into(),
                ],
                if runs >= 3 { "high" } else { "medium" },
                "medium",
                "low",
                match per_run {
                    Some(ns) => json!({ "kind": "measured", "avoidable_time_ns": ns, "basis": "duration of one script run in this build; saved each time an unrelated edit no longer triggers it (the dependent crate's recompilation comes on top)" }),
                    None => unknown_impact("the script's duration could not be measured in this run"),
                },
                "declare the script's real inputs, edit an unrelated file in the package, rebuild, and check `haul analyze scripts` shows the script as not run",
                "only safe when every file, environment variable and external tool the script reads is declared; an incomplete declaration makes builds stale",
                "review_required",
            )
            .scenarios(&["small-edit", "no-change"]),
        );
    }

    // Several versions of one dependency that the lockfile could unify (FR-050.1).
    for d in report["duplication"]["versions"].as_array().into_iter().flatten() {
        if d["state"] != json!("potentially-reducible") {
            continue;
        }
        let versions = d["versions"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let costs: Vec<u64> = versions.iter().filter_map(|v| v["observed_compile_ns"].as_u64()).collect();
        let impact = if costs.len() == versions.len() && !costs.is_empty() {
            json!({ "kind": "measured", "avoidable_time_ns": costs.iter().sum::<u64>() - costs.iter().max().copied().unwrap_or(0), "basis": "observed compile time of all but the most expensive version, on a clean build" })
        } else {
            unknown_impact("not every version was observed compiling, so the saving cannot be stated")
        };
        let mut evidence: Vec<String> = versions.iter().map(|v| format!("{} is required via {}", vstr(v, "version"), v["required_by"].as_array().map(|p| p.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" → ")).unwrap_or_default())).collect();
        evidence.push("the versions are SemVer-compatible according to Cargo's resolution".into());
        out.push(
            rec(
                format!("rec:duplicate-versions:{}", vstr(d, "name")),
                "duplicate_dependency_versions",
                "dependencies",
                format!("`{}` is built in {} compatible versions", vstr(d, "name"), versions.len()),
                evidence,
                "medium",
                "medium",
                "low",
                impact,
                "run `cargo update -p <name>` (or align the requirements), then `cargo tree -d` and the project's tests",
                "a dependent may pin an older version on purpose (`=x.y.z` or an upper bound); the lockfile change must be reviewed",
                "review_required",
            )
            .scenarios(&["clean"]),
        );
    }

    // One version compiled with several feature sets (FR-050.2, FR-050.6).
    for g in report["duplication"]["variants"].as_array().into_iter().flatten() {
        let causes: Vec<&str> = g["causes"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let variants = g["variants"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        // Advice about features is only given when the dependents requesting each variant are known.
        let dependents_known = variants.iter().all(|v| v["dependents"].as_array().map(|d| !d.is_empty()).unwrap_or(false));
        if causes != ["feature-set"] || !dependents_known {
            continue;
        }
        let durs: Vec<u64> = variants.iter().filter_map(|v| v["dur_ns"].as_u64()).collect();
        let evidence = variants
            .iter()
            .map(|v| {
                format!(
                    "features [{}] requested by {}",
                    v["features"].as_array().map(|f| f.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
                    v["dependents"].as_array().map(|f| f.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default()
                )
            })
            .collect();
        out.push(
            rec(
                format!("rec:feature-variants:{}@{}", vstr(g, "package"), vstr(g, "version")),
                "feature_variant_duplication",
                "dependencies",
                format!("`{}` {} is compiled {} times with different feature sets", vstr(g, "package"), vstr(g, "version"), variants.len()),
                evidence,
                "medium",
                "medium",
                "medium",
                json!({ "kind": "measured", "avoidable_time_ns": durs.iter().sum::<u64>() - durs.iter().max().copied().unwrap_or(0), "basis": "observed compile time of all but the most expensive variant" }),
                "make the listed dependents request the same features (for example through `[workspace.dependencies]`), rebuild, and check `haul analyze duplication`",
                "enabling a feature for every dependent changes what those crates compile against; disabling one may remove functionality a dependent needs",
                "review_required",
            )
            .scenarios(&["clean"]),
        );
    }

    // Full debug information for dependencies in development builds (FR-080.4).
    let dev_debug: Vec<&Value> = report["codegen"]["settings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["settings"]["class"] == json!("development") && matches!(s["settings"]["debuginfo"].as_str(), Some("2") | Some("full")) && s["dependency_units"].as_u64().unwrap_or(0) > 0)
        .collect();
    if !dev_debug.is_empty() {
        let units: u64 = dev_debug.iter().filter_map(|s| s["dependency_units"].as_u64()).sum();
        let time: u64 = dev_debug.iter().filter_map(|s| s["time_ns"].as_u64()).sum();
        out.push(
            rec(
                "rec:dev-debuginfo:dependencies".into(),
                "dev_dependency_debuginfo",
                "codegen",
                "Dependencies are compiled with full debug information in the dev profile".into(),
                vec![
                    format!("{} dependency unit(s) were compiled with -C debuginfo=2 ({} of compile time in those settings)", units, human_dur(time)),
                    "Cargo's build-performance guidance suggests `debug = \"line-tables-only\"` for the workspace and `debug = false` for dependencies".into(),
                ],
                "medium",
                "low",
                "low",
                unknown_impact("the saving depends on how much of this build is debug-info generation and linking; measure it with the experiment below"),
                "haul experiment --set 'profile.dev.package.\"*\".debug=false' --set profile.dev.debug=\"line-tables-only\" --scenario clean -- build",
                "debugger experience changes: no variable or type information inside dependencies, line numbers only in workspace crates; backtraces keep working. Release profiles are untouched",
                "review_required",
            )
            .scenarios(&["clean", "small-edit"])
            .with("tradeoff", json!({ "build_and_storage": "less debug data to generate, link and store", "debugging": "stepping through dependencies and inspecting variables needs a profile with full debug info" })),
        );
    }

    // Incremental compilation versus sccache: one choice, two alternatives (FR-090, FR-150.3).
    let inc = &report["policy"]["incremental"];
    if inc["sccache_conflict"] == json!(true) {
        let n = inc["observed"]["units_incremental"].as_u64().unwrap_or(0);
        let keep = json!({
            "id": "keep-incremental",
            "action": "keep incremental compilation; sccache then only serves dependency crates",
            "suits": "a developer editing one checkout repeatedly",
            "default_for": "local-dev",
        });
        let disable = json!({
            "id": "disable-incremental",
            "action": "set incremental = \"off\" for this workload so sccache can cache workspace crates",
            "suits": "clean or ephemeral runners that cannot reuse incremental state",
            "default_for": Value::Null,
            "config": format!("[incremental]\n{} = \"off\"", workload),
        });
        out.push(
            rec(
                "rec:cache-strategy:incremental-vs-sccache".into(),
                "cache_strategy_choice",
                "cache",
                "Incremental compilation and sccache are competing for the same units".into(),
                vec![
                    format!("{} unit(s) were compiled incrementally while sccache was the active rustc wrapper", n),
                    "sccache cannot cache rustc invocations that use incremental compilation".into(),
                    format!("workload class: {}", workload),
                ],
                "high",
                "low",
                "low",
                unknown_impact("which strategy is faster depends on the workload; neither is assumed"),
                "haul experiment --env CARGO_INCREMENTAL=0 --scenario small-edit -- build   (and the same with --scenario clean for CI-like runs)",
                "disabling incremental compilation slows repeated edits in a persistent checkout; it is not recommended for local development unless the experiment shows a win",
                "safe",
            )
            .scenarios(&["small-edit", "clean"])
            .with("alternatives", json!(if workload == "ci" { vec![disable, keep] } else { vec![keep, disable] }))
            .with("choice", json!("experiment: pick one alternative per workload; they are mutually exclusive")),
        );
    }

    // A cache that mostly misses still costs lookups and writes (FR-100.1, OBJ-024).
    let cache = &report["cache"];
    if let (Some(hits), Some(misses)) = (cache["hits"].as_u64(), cache["misses"].as_u64()) {
        let cacheable = hits + misses;
        if cacheable >= 20 && hits * 10 < cacheable * 3 {
            out.push(
                rec(
                    "rec:cache:low-hit-rate".into(),
                    "cache_low_hit_rate",
                    "cache",
                    format!("sccache served {}/{} cacheable requests", hits, cacheable),
                    vec![
                        format!("{} hits, {} misses, {} non-cacheable out of {} compile requests", hits, misses, cache["non_cacheable"].as_u64().unwrap_or(0), cache["requests"].as_u64().unwrap_or(0)),
                        format!("cache lookups took {}, writes {}", human_dur(cache["lookup_time_ns"].as_u64().unwrap_or(0)), human_dur(cache["write_time_ns"].as_u64().unwrap_or(0))),
                    ],
                    "medium",
                    "low",
                    "medium",
                    unknown_impact("a first build after a toolchain or flag change legitimately misses; compare a second run before acting"),
                    "repeat the same build from a clean target directory and compare `haul cache status`; hits should dominate on the second run",
                    "differing absolute checkout paths, RUSTFLAGS or toolchains fragment cache keys; none of them may be normalised silently",
                    "none",
                )
                .scenarios(&["clean"]),
            );
        }
    }

    // One unit dominating the critical path (FR-030.3).
    let cp_total = report["metrics"]["critical_path_ns"].as_u64().unwrap_or(0);
    if let Some(top) = report["critical_path"].as_array().into_iter().flatten().max_by_key(|u| u["duration_ns"].as_u64().unwrap_or(0)) {
        let d = top["duration_ns"].as_u64().unwrap_or(0);
        if d >= 2_000_000_000 && cp_total > 0 && d * 10 >= cp_total * 3 {
            let is_macro = top["kind"] == json!("proc-macro");
            out.push(
                rec(
                    format!("rec:critical-path:{}", vstr(top, "unit_id")),
                    "critical_path_bottleneck",
                    "structure",
                    format!("`{}` is {}% of the critical path", vstr(top, "package"), d * 100 / cp_total),
                    vec![
                        format!("unit duration {} on a critical path of {}", human_dur(d), human_dur(cp_total)),
                        format!("critical-path model: {}", vstr(&report["critical_path_model"], "completeness")),
                        if is_macro { "it is a proc-macro crate: everything that uses the macro waits for it".to_string() } else { "units that depend on it cannot start their own codegen-dependent work earlier".to_string() },
                    ],
                    if report["critical_path_model"]["completeness"] == json!("observed") { "medium" } else { "low" },
                    "medium",
                    "high",
                    json!({ "kind": "modeled", "avoidable_time_ns": d, "basis": "upper bound: the unit's whole duration, reached only if it left the critical path entirely" }),
                    "after restructuring, compare `haul compare` critical_path_ns on a clean build",
                    "splitting a crate or trimming its dependencies is a source-architecture decision; Haul never rewrites source layout",
                    "none",
                )
                .scenarios(&["clean"]),
            );
        }
    }

    // The same unit rebuilt again and again for a configuration reason (FR-040.3).
    const FLIP: &[&str] = &["rustflags", "environment", "profile", "feature-set", "compiler", "target", "build-script-input"];
    let mut recurring: BTreeMap<String, (u64, u64, Vec<String>)> = BTreeMap::new();
    for entries in index.rebuild_log().as_object().into_iter().flat_map(|o| o.values()) {
        for e in entries.as_array().into_iter().flatten() {
            let reason = e["reason"].as_str().unwrap_or("");
            if FLIP.contains(&reason) {
                let slot = recurring.entry(reason.to_string()).or_default();
                slot.0 += 1;
                slot.1 += e["cost_ns"].as_u64().unwrap_or(0);
                let unit = vstr(e, "unit");
                if !slot.2.contains(&unit) && slot.2.len() < 6 {
                    slot.2.push(unit);
                }
            }
        }
    }
    let this_run: BTreeMap<&str, u64> = report["rebuilds"]["summary"]["by_reason"].as_object().into_iter().flatten().map(|(k, v)| (k.as_str(), v["units"].as_u64().unwrap_or(0))).collect();
    for (reason, (count, cost, units)) in recurring {
        // Earlier runs plus this one: a pattern needs repetition and a material cost.
        let total = count + this_run.get(reason.as_str()).copied().unwrap_or(0);
        if total < 6 || cost < 2_000_000_000 || !this_run.contains_key(reason.as_str()) {
            continue;
        }
        let hint = match reason.as_str() {
            "rustflags" | "environment" | "profile" | "feature-set" => "typical cause: two tools (an IDE and the command line, or two scripts) building the same target directory with different settings",
            "compiler" | "target" => "typical cause: alternating toolchains or targets in one target directory",
            _ => "typical cause: a build script whose inputs change on every build",
        };
        out.push(
            rec(
                format!("rec:recurring-invalidation:{}", reason),
                "recurring_invalidation",
                "invalidation",
                format!("Units keep being rebuilt because of `{}` changes", reason),
                vec![format!("{} rebuild(s) with this cause across recent runs, costing {}", total, human_dur(cost)), format!("affected: {}", units.join(", ")), hint.to_string()],
                "medium",
                "low",
                "medium",
                json!({ "kind": "measured", "avoidable_time_ns": cost, "basis": "compile time already spent on these rebuilds in recorded runs" }),
                "run `haul analyze rebuilds` after the next few builds; the cause should disappear from the summary",
                "align the settings between the tools involved, or give them separate target directories; Haul does not normalise flags or environment on its own",
                "none",
            )
            .scenarios(&["small-edit", "no-change"]),
        );
    }

    // Reclaimable build output (FR-110).
    for inv in report["artifacts"].as_array().into_iter().flatten() {
        let bytes = inv["gc_plan"]["candidate_bytes"].as_u64().unwrap_or(0);
        if bytes >= 256 * 1024 * 1024 {
            out.push(
                rec(
                    format!("rec:stale-artifacts:{}", &crate::util::sha256_hex(vstr(inv, "root").as_bytes())[..8]),
                    "stale_artifacts",
                    "storage",
                    format!("{} of stale build output can be reclaimed", human_bytes(bytes)),
                    vec![
                        format!("{} group(s) are rebuildable and unused for more than {} days", inv["gc_plan"]["candidate_groups"].as_u64().unwrap_or(0), inv["gc_plan"]["stale_days"].as_u64().unwrap_or(0)),
                        format!("directory total: {}", human_bytes(inv["allocated_bytes"].as_u64().unwrap_or(0))),
                    ],
                    "medium",
                    "low",
                    "low",
                    json!({ "kind": "measured", "reclaimable_bytes": bytes, "basis": "allocated bytes of the candidate groups in the dry-run plan" }),
                    "haul gc --dry-run",
                    "this release plans only; `cargo clean` keeps its exact upstream meaning and removes reusable artifacts as well",
                    "none",
                )
                .scenarios(&["storage"]),
            );
        }
    }

    // Linking units dominating compile time (FR-080).
    let link_ns = report["codegen"]["linking"]["linking_unit_time_ns"].as_u64().unwrap_or(0);
    let sum_ns = report["metrics"]["sum_unit_time_ns"].as_u64().unwrap_or(0);
    let default_linker = report["codegen"]["linking"]["units"].as_array().map(|u| u.iter().all(|l| l["linker"] == json!("platform default"))).unwrap_or(false);
    if link_ns >= 2_000_000_000 && sum_ns > 0 && link_ns * 4 >= sum_ns && default_linker {
        out.push(
            rec(
                "rec:linker:experiment".into(),
                "linking_unit_share",
                "linker",
                format!("Units that link take {}% of compile time with the platform default linker", link_ns * 100 / sum_ns),
                vec![format!("linking units: {} of {} total unit time", human_dur(link_ns), human_dur(sum_ns)), "the linker's own share inside those units is not observable on a stable toolchain".into()],
                "low",
                "medium",
                "low",
                unknown_impact("link time is not separable from codegen here; only an experiment can tell"),
                "haul experiment linker --linker lld --scenario small-edit --verify 'test' -- build",
                "a different linker must pass the project's tests before it counts as verified; platform and native-library support varies",
                "review_required",
            )
            .scenarios(&["small-edit"]),
        );
    }

    let mut recs: Vec<Value> = out
        .into_iter()
        .map(|r| {
            let mut v = r.0;
            v["score"] = json!((score(&v) * 1000.0).round() / 1000.0);
            v
        })
        .collect();
    recs.sort_by(|a, b| b["score"].as_f64().partial_cmp(&a["score"].as_f64()).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a["id"].as_str().cmp(&b["id"].as_str())));
    recs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Value {
        json!({ "policy": { "workload": "local-dev", "incremental": {} }, "build_scripts": [], "duplication": {}, "codegen": {}, "cache": {}, "metrics": {}, "critical_path": [], "critical_path_model": {}, "rebuilds": {}, "artifacts": null })
    }

    #[test]
    fn nothing_is_recommended_without_evidence() {
        assert!(generate(&base(), &Index::empty()).is_empty());
    }

    #[test]
    fn unmeasured_gains_carry_no_number() {
        // FR-150 acceptance: no measured evidence ⇒ no "expected improvement" figure.
        let mut r = base();
        r["codegen"] = json!({ "settings": [{ "settings": { "class": "development", "debuginfo": "2" }, "dependency_units": 40, "time_ns": 9_000_000_000u64 }] });
        let recs = generate(&r, &Index::empty());
        assert_eq!(recs.len(), 1);
        let impact = &recs[0]["impact"];
        assert_eq!(impact["kind"], "unknown");
        assert!(impact.get("avoidable_time_ns").is_none());
        assert!(!recs[0]["title"].as_str().unwrap().contains('%'));
        for field in ["category", "evidence", "impacted_scenarios", "confidence", "risk", "effort", "verification", "compatibility"] {
            assert!(!recs[0][field].is_null(), "every recommendation carries {}", field);
        }
    }

    #[test]
    fn exclusive_cache_strategies_are_one_choice() {
        // FR-150 acceptance: mutually exclusive strategies appear as an experiment choice.
        let mut r = base();
        r["policy"]["incremental"] = json!({ "sccache_conflict": true, "observed": { "units_incremental": 4 } });
        let recs = generate(&r, &Index::empty());
        assert_eq!(recs.len(), 1);
        let alts = recs[0]["alternatives"].as_array().unwrap();
        assert_eq!(alts.len(), 2);
        // FR-090 acceptance: on a developer machine, keeping incremental is the default alternative.
        assert_eq!(alts[0]["id"], "keep-incremental");
        assert!(recs[0]["compatibility"].as_str().unwrap().contains("not recommended for local development"));
        r["policy"]["workload"] = json!("ci");
        assert_eq!(generate(&r, &Index::empty())[0]["alternatives"][0]["id"], "disable-incremental");
    }

    #[test]
    fn measured_recommendations_rank_above_unknown_ones() {
        let mut r = base();
        r["codegen"] = json!({ "settings": [{ "settings": { "class": "development", "debuginfo": "2" }, "dependency_units": 40, "time_ns": 1 }] });
        r["build_scripts"] = json!([{ "state": "over-invalidating-candidate", "package": "gen", "duration_ns": 4_000_000_000u64, "history": { "runs": 3, "total_ns": 12_000_000_000u64 } }]);
        let recs = generate(&r, &Index::empty());
        assert_eq!(recs[0]["rule"], "build_script_broad_invalidation");
        assert_eq!(recs[0]["impact"]["kind"], "measured");
        assert_eq!(recs[0]["remediation"], "review_required", "rerun-if directives are never classified safe (FR-060.3)");
        assert!(recs[0]["score"].as_f64().unwrap() > recs[1]["score"].as_f64().unwrap());
    }

    #[test]
    fn feature_advice_requires_known_dependents() {
        let variant = |features: Value, dependents: Value| json!({ "features": features, "dependents": dependents, "dur_ns": 1_000_000_000u64 });
        let mut r = base();
        r["duplication"] = json!({ "variants": [{ "package": "a", "version": "1.0.0", "causes": ["feature-set"], "variants": [variant(json!([]), json!(["x"])), variant(json!(["std"]), json!([]))] }] });
        assert!(generate(&r, &Index::empty()).is_empty(), "no feature recommendation when a variant's dependents are unknown (FR-050.6)");
        r["duplication"]["variants"][0]["variants"][1]["dependents"] = json!(["y"]);
        let recs = generate(&r, &Index::empty());
        assert_eq!(recs[0]["rule"], "feature_variant_duplication");
        assert_eq!(recs[0]["impact"]["avoidable_time_ns"], 1_000_000_000u64);
        assert!(recs[0]["evidence"][1].as_str().unwrap().contains("requested by y"));
    }
}
