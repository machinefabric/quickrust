//! Comparison engine (FR-160), baselines and regression budgets (FR-130).
//!
//! Two subjects are only compared after their environments have been compared;
//! differences are reported before any delta and are never hidden (FR-160.3).

use crate::config::Config;
use crate::store::WsStore;
use crate::util::{self, vpath};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Metrics that can be compared and budgeted, with their unit (DATA-002).
pub const METRICS: &[(&str, &str)] = &[
    ("wall_time_ns", "ns"),
    ("critical_path_ns", "ns"),
    ("sum_unit_time_ns", "ns"),
    ("cpu_time_ns", "ns"),
    ("peak_rss_bytes", "bytes"),
    ("units_compiled", "count"),
    ("artifact_allocated_bytes", "bytes"),
    ("artifact_logical_bytes", "bytes"),
    ("output_bytes_written", "bytes"),
    ("cache_hits", "count"),
    ("cache_misses", "count"),
    ("cache_non_cacheable", "count"),
    ("cache_hit_ratio", "ratio"),
    ("cache_lookup_time_ns", "ns"),
];

fn metric_unit(name: &str) -> Option<&'static str> {
    METRICS.iter().find(|(m, _)| *m == name).map(|(_, u)| *u)
}

/// The comparable essence of a finalized run report.
pub fn summarize_run(report: &Value) -> Value {
    let m = &report["metrics"];
    let c = &report["cache"];
    let ratio = match (c["hits"].as_u64(), c["misses"].as_u64()) {
        (Some(h), Some(mi)) if h + mi > 0 => json!(h as f64 / (h + mi) as f64),
        _ => Value::Null,
    };
    json!({
        "schema_version": crate::SCHEMA_VERSION,
        "kind": "run",
        "id": report["run"]["id"],
        "label": format!("run {}", util::vstr(&report["run"], "id")),
        "created_at": report["run"]["started_at"],
        "scenario": {
            "name": report["run"]["scenario"]["name"].as_str().map(str::to_string).unwrap_or_else(|| format!("adhoc:{}", util::vstr(&report["run"]["command"], "family"))),
            "cache_state": report["run"]["scenario"]["cache_state"],
        },
        "environment": {
            "rustc_version": report["toolchain"]["rustc_version"],
            "cargo_version": report["toolchain"]["cargo_version"],
            "host": report["toolchain"]["host"],
            "machine_class": report["environment"]["machine"]["class"],
            "os": report["environment"]["machine"]["os"],
            "cpu_model": report["environment"]["machine"]["cpu_model"],
            "logical_cpus": report["environment"]["machine"]["logical_cpus"],
            "variables_digest": report["environment"]["variables"]["digest"],
            "command_family": report["run"]["command"]["family"],
            "profile": report["run"]["command"]["profile"],
            "targets": report["run"]["command"]["targets"],
            "source_revision": report["workspace"]["source_revision"],
            "dirty": report["workspace"]["dirty"],
            "lockfile_digest": report["workspace"]["lockfile_digest"],
            "cache_provider": c["provider"],
        },
        "metrics": {
            "wall_time_ns": m["wall_time_ns"],
            "critical_path_ns": m["critical_path_ns"],
            "sum_unit_time_ns": m["sum_unit_time_ns"],
            "cpu_time_ns": m["cpu_time_ns"],
            "peak_rss_bytes": m["peak_rss_bytes"],
            "units_compiled": m["units_compiled"],
            "artifact_allocated_bytes": m["artifact_allocated_bytes"],
            "artifact_logical_bytes": m["artifact_logical_bytes"],
            "output_bytes_written": m["output_bytes_written"],
            "cache_hits": c["hits"],
            "cache_misses": c["misses"],
            "cache_non_cacheable": c["non_cacheable"],
            "cache_hit_ratio": ratio,
            "cache_lookup_time_ns": c["lookup_time_ns"],
        },
        "samples": {},
        "succeeded": report["run"]["result"]["cargo_exit_code"] == json!(0),
    })
}

/// Groups run summaries by toolchain and machine so that only like is aggregated with like (FR-020).
/// Returns the largest group and the summaries left out of it.
pub fn homogeneous_group(summaries: Vec<Value>) -> (Vec<Value>, Vec<Value>) {
    let key = |s: &Value| format!("{}|{}|{}", s["environment"]["rustc_version"], s["environment"]["host"], s["environment"]["machine_class"]);
    let mut counts: Vec<(String, usize)> = Vec::new();
    for s in &summaries {
        let k = key(s);
        match counts.iter_mut().find(|(c, _)| *c == k) {
            Some(c) => c.1 += 1,
            None => counts.push((k, 1)),
        }
    }
    // Ties go to the group seen first, so a single stray sample never wins.
    let mut best: Option<(String, usize)> = None;
    for (k, n) in counts {
        if best.as_ref().map(|(_, bn)| n > *bn).unwrap_or(true) {
            best = Some((k, n));
        }
    }
    let Some((best, _)) = best else { return (Vec::new(), Vec::new()) };
    summaries.into_iter().partition(|s| key(s) == best)
}

fn stats(values: &[u64]) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let n = values.len() as f64;
    let mean = values.iter().map(|v| *v as f64).sum::<f64>() / n;
    let var = values.iter().map(|v| (*v as f64 - mean).powi(2)).sum::<f64>() / n;
    json!({
        "n": values.len(), "min": values.iter().min(), "max": values.iter().max(), "median": util::median(values),
        "mean": mean.round() as u64, "stddev": var.sqrt().round() as u64,
        "relative_stddev_percent": if mean > 0.0 { json!((var.sqrt() / mean * 1000.0).round() / 10.0) } else { Value::Null },
    })
}

/// Aggregates the measured runs of one benchmark scenario: medians, with raw samples kept (FR-020.4).
pub fn summarize_group(id: &str, scenario: &Value, runs: &[Value]) -> Value {
    let Some(first) = runs.first() else { return Value::Null };
    let mut metrics = serde_json::Map::new();
    let mut samples = serde_json::Map::new();
    let mut distributions = serde_json::Map::new();
    for (name, unit) in METRICS {
        if *unit == "ratio" {
            let vals: Vec<f64> = runs.iter().filter_map(|r| r["metrics"][name].as_f64()).collect();
            metrics.insert((*name).into(), if vals.len() == runs.len() { json!(vals.iter().sum::<f64>() / vals.len() as f64) } else { Value::Null });
            continue;
        }
        let vals: Vec<u64> = runs.iter().filter_map(|r| r["metrics"][name].as_u64()).collect();
        // A metric missing from any sample is unavailable for the group, not silently averaged.
        if vals.len() == runs.len() {
            metrics.insert((*name).into(), json!(util::median(&vals)));
            distributions.insert((*name).into(), stats(&vals));
            samples.insert((*name).into(), json!(vals));
        } else {
            metrics.insert((*name).into(), Value::Null);
        }
    }
    json!({
        "schema_version": crate::SCHEMA_VERSION,
        "kind": "benchmark",
        "id": id,
        "label": format!("benchmark {} ({})", id, util::vstr(scenario, "name")),
        "created_at": util::iso8601(util::now_wall_ns()),
        "scenario": scenario,
        "environment": first["environment"],
        "metrics": metrics,
        "samples": samples,
        "distributions": distributions,
        "runs": runs.iter().map(|r| r["id"].clone()).collect::<Vec<_>>(),
        "succeeded": runs.iter().all(|r| r["succeeded"] == json!(true)),
    })
}

/// Compares the environments of two summaries: `(state, differences)`.
pub fn comparability(base: &Value, cand: &Value) -> (&'static str, Vec<Value>) {
    // Differences that make a performance delta meaningless.
    const BLOCKING: &[(&str, &str)] = &[
        ("environment.rustc_version", "compiler version"),
        ("environment.host", "host triple"),
        ("environment.machine_class", "hardware class"),
        ("environment.command_family", "command"),
        ("environment.profile", "profile"),
        ("environment.targets", "compilation targets"),
        ("scenario.name", "scenario"),
        ("scenario.cache_state", "cache state (cold vs warm)"),
    ];
    // Differences worth knowing about that may well be the thing under test.
    const NOTABLE: &[(&str, &str)] = &[
        ("environment.cargo_version", "Cargo version"),
        ("environment.variables_digest", "build-relevant environment variables"),
        ("environment.cache_provider", "compiler cache"),
        ("environment.source_revision", "source revision"),
        ("environment.dirty", "working-tree dirtiness"),
        ("environment.lockfile_digest", "lockfile"),
    ];
    let mut diffs = Vec::new();
    let mut state = "comparable";
    for (set, severity) in [(BLOCKING, "blocking"), (NOTABLE, "notable")] {
        for (path, what) in set {
            let (a, b) = (vpath(base, path).unwrap_or(&Value::Null), vpath(cand, path).unwrap_or(&Value::Null));
            if a != b {
                diffs.push(json!({ "field": path, "what": what, "baseline": a, "candidate": b, "severity": severity }));
                state = match (state, severity) {
                    (_, "blocking") => "incomparable",
                    ("comparable", _) => "partially-comparable",
                    (s, _) => s,
                };
            }
        }
    }
    (state, diffs)
}

pub fn compare(base: &Value, cand: &Value) -> Value {
    let (state, differences) = comparability(base, cand);
    let mut metrics = Vec::new();
    for (name, unit) in METRICS {
        let (a, b) = (base["metrics"][name].as_f64(), cand["metrics"][name].as_f64());
        let mut row = json!({ "metric": name, "unit": unit, "baseline": base["metrics"][name], "candidate": cand["metrics"][name] });
        match (a, b) {
            (Some(a), Some(b)) => {
                row["available"] = json!(true);
                row["delta"] = if *unit == "ratio" { json!(b - a) } else { json!((b - a) as i64) };
                row["delta_percent"] = if a != 0.0 { json!(((b - a) / a * 1000.0).round() / 10.0) } else { Value::Null };
            }
            _ => {
                // FR-160: a missing metric is shown as unavailable, never as zero.
                row["available"] = json!(false);
                row["unavailable_in"] = json!(match (a.is_some(), b.is_some()) {
                    (false, false) => "both",
                    (false, true) => "baseline",
                    _ => "candidate",
                });
            }
        }
        if let (Some(da), Some(db)) = (base["distributions"].get(*name), cand["distributions"].get(*name)) {
            row["baseline_distribution"] = da.clone();
            row["candidate_distribution"] = db.clone();
        }
        metrics.push(row);
    }
    json!({
        "schema_version": crate::SCHEMA_VERSION,
        "baseline": { "id": base["id"], "label": base["label"], "kind": base["kind"], "created_at": base["created_at"], "succeeded": base["succeeded"] },
        "candidate": { "id": cand["id"], "label": cand["label"], "kind": cand["kind"], "created_at": cand["created_at"], "succeeded": cand["succeeded"] },
        "comparability": { "state": state, "differences": differences },
        "metrics": metrics,
    })
}

pub fn baseline_dir(ws_root: &Path) -> PathBuf {
    ws_root.join(".haul").join("baselines")
}

fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) || name.starts_with('.') {
        return Err(format!("invalid baseline name {:?}: use letters, digits, `-`, `_` and `.`", name));
    }
    Ok(())
}

/// Baselines live in the repository so that CI and reviewers see the same approved numbers (CFG-006).
pub fn save_baseline(ws_root: &Path, name: &str, summary: &Value) -> Result<PathBuf, String> {
    valid_name(name)?;
    let mut v = summary.clone();
    v["baseline"] = json!({ "name": name, "promoted_at": util::iso8601(util::now_wall_ns()), "from": summary["id"] });
    v["label"] = json!(format!("baseline {}", name));
    let path = baseline_dir(ws_root).join(format!("{}.json", name));
    util::write_json(&path, &v).map_err(|e| format!("could not write baseline {}: {}", path.display(), e))?;
    Ok(path)
}

pub fn load_baseline(ws_root: &Path, name: &str) -> Result<Value, String> {
    valid_name(name)?;
    let path = baseline_dir(ws_root).join(format!("{}.json", name));
    let v = util::read_json(&path).ok_or_else(|| format!("baseline `{}` not found or unreadable at {} (create it with `haul baseline set {}`)", name, path.display(), name))?;
    let major = |s: &str| s.split('.').next().unwrap_or("").to_string();
    if major(v["schema_version"].as_str().unwrap_or("")) != major(crate::SCHEMA_VERSION) {
        return Err(format!("baseline `{}` uses schema {} but this Haul reads {}", name, v["schema_version"], crate::SCHEMA_VERSION));
    }
    Ok(v)
}

pub fn list_baselines(ws_root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(baseline_dir(ws_root))
        .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string)).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Resolves a comparison subject: `baseline:NAME`, `benchmark:ID`, or a run reference.
pub fn resolve_subject(store: &WsStore, cfg: &Config, reference: &str) -> Result<Value, String> {
    if let Some(name) = reference.strip_prefix("baseline:") {
        let root = store.root.as_ref().ok_or("baselines need a Cargo workspace")?;
        return load_baseline(root, name);
    }
    if let Some(id) = reference.strip_prefix("benchmark:") {
        return util::read_json(&store.benchmarks_dir().join(format!("{}.json", id))).map(|b| b["summary"].clone()).filter(|s| s.is_object()).ok_or_else(|| format!("benchmark `{}` not found", id));
    }
    let id = store.resolve_run_ref(reference, false).map_err(|e| format!("{} (baselines are referenced as `baseline:NAME`, benchmarks as `benchmark:ID`)", e))?;
    Ok(summarize_run(&crate::analyze::finalize(store, &id, cfg)?))
}

#[derive(Debug, Clone)]
pub struct Budget {
    pub metric: String,
    pub scenario: Option<String>,
    pub max_value: Option<f64>,
    pub min_value: Option<f64>,
    pub max_increase_percent: Option<f64>,
    pub max_decrease_percent: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct Budgets {
    pub blocking: bool,
    pub incomparable_fails: bool,
    pub noise_percent: f64,
    pub budgets: Vec<Budget>,
    pub source: String,
}

fn threshold(v: &toml::Value, unit: &str, field: &str) -> Result<f64, String> {
    match v {
        toml::Value::Integer(i) => Ok(*i as f64),
        toml::Value::Float(f) => Ok(*f),
        toml::Value::String(s) => match unit {
            "ns" => util::parse_duration_ns(s).map(|v| v as f64),
            "bytes" => util::parse_bytes(s).map(|v| v as f64),
            _ => s.parse().ok(),
        }
        .ok_or_else(|| format!("`{}` = {:?} is not a valid {} value", field, s, unit)),
        _ => Err(format!("`{}` must be a number or a string with a unit", field)),
    }
}

/// Parses `.haul/budgets.toml`. Unknown keys and metrics are errors: a budget that
/// silently does not apply is worse than none.
pub fn parse_budgets(text: &str, source: &str) -> Result<Budgets, String> {
    let doc: toml::Table = text.parse().map_err(|e| format!("invalid TOML in {}: {}", source, e))?;
    let mut out = Budgets { blocking: false, incomparable_fails: false, noise_percent: 0.0, budgets: Vec::new(), source: source.to_string() };
    for (key, value) in &doc {
        match key.as_str() {
            "policy" => {
                for (k, v) in value.as_table().ok_or_else(|| format!("`policy` in {} must be a table", source))? {
                    match (k.as_str(), v) {
                        // Warn-only is the default so budgets can be rolled out before they block (FR-130.4).
                        ("mode", toml::Value::String(s)) if s == "warn" || s == "blocking" => out.blocking = s == "blocking",
                        ("incomparable", toml::Value::String(s)) if s == "warn" || s == "fail" => out.incomparable_fails = s == "fail",
                        ("noise-percent", v) if v.is_float() || v.is_integer() => out.noise_percent = v.as_float().or(v.as_integer().map(|i| i as f64)).unwrap_or(0.0).max(0.0),
                        _ => return Err(format!("invalid `policy.{}` in {}", k, source)),
                    }
                }
            }
            "budget" => {
                for (n, b) in value.as_array().ok_or_else(|| format!("`budget` in {} must be an array of tables (`[[budget]]`)", source))?.iter().enumerate() {
                    let t = b.as_table().ok_or_else(|| format!("budget #{} in {} must be a table", n + 1, source))?;
                    let metric = t.get("metric").and_then(|m| m.as_str()).ok_or_else(|| format!("budget #{} in {} needs a `metric`", n + 1, source))?;
                    let unit = metric_unit(metric).ok_or_else(|| format!("budget #{} in {}: unknown metric `{}` (known: {})", n + 1, source, metric, METRICS.iter().map(|(m, _)| *m).collect::<Vec<_>>().join(", ")))?;
                    let mut budget = Budget { metric: metric.to_string(), scenario: None, max_value: None, min_value: None, max_increase_percent: None, max_decrease_percent: None };
                    for (k, v) in t {
                        let field = format!("budget #{} `{}`", n + 1, k);
                        match k.as_str() {
                            "metric" => {}
                            "scenario" => budget.scenario = Some(v.as_str().ok_or_else(|| format!("{} must be a string", field))?.to_string()),
                            "max-value" => budget.max_value = Some(threshold(v, unit, &field)?),
                            "min-value" => budget.min_value = Some(threshold(v, unit, &field)?),
                            "max-increase-percent" => budget.max_increase_percent = Some(threshold(v, "percent", &field)?),
                            "max-decrease-percent" => budget.max_decrease_percent = Some(threshold(v, "percent", &field)?),
                            _ => return Err(format!("unknown key `{}` in budget #{} of {}", k, n + 1, source)),
                        }
                    }
                    if budget.max_value.is_none() && budget.min_value.is_none() && budget.max_increase_percent.is_none() && budget.max_decrease_percent.is_none() {
                        return Err(format!("budget #{} in {} sets no threshold", n + 1, source));
                    }
                    out.budgets.push(budget);
                }
            }
            other => return Err(format!("unknown key `{}` in {}", other, source)),
        }
    }
    Ok(out)
}

pub fn load_budgets(ws_root: &Path, cfg: &Config) -> Result<Budgets, String> {
    let path = ws_root.join(cfg.str("ci.budget-policy"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read budget policy {}: {}", path.display(), e))?;
    parse_budgets(&text, &path.display().to_string())
}

/// Evaluates budgets for a candidate, against a baseline where thresholds are relative.
pub fn evaluate(budgets: &Budgets, baseline: Option<&Value>, candidate: &Value) -> Value {
    let comp = baseline.map(|b| comparability(b, candidate));
    let scenario = util::vstr(&candidate["scenario"], "name");
    let mut evals = Vec::new();
    let (mut failed, mut incomparable) = (false, false);
    for b in &budgets.budgets {
        if b.scenario.as_ref().map(|s| *s != scenario).unwrap_or(false) {
            continue;
        }
        let cand = candidate["metrics"][&b.metric].as_f64();
        let base = baseline.and_then(|bl| bl["metrics"][&b.metric].as_f64());
        let mut checks: Vec<Value> = Vec::new();
        let mut verdict = "pass";
        let worst = |v: &'static str, verdict: &mut &'static str| {
            let rank = |s: &str| match s {
                "fail" => 3,
                "incomparable" => 2,
                "unavailable" => 1,
                _ => 0,
            };
            if rank(v) > rank(verdict) {
                *verdict = v;
            }
        };
        for (kind, limit) in [("max-value", b.max_value), ("min-value", b.min_value)] {
            let Some(limit) = limit else { continue };
            let result = match cand {
                None => "unavailable",
                Some(c) if (kind == "max-value" && c > limit) || (kind == "min-value" && c < limit) => "fail",
                Some(_) => "pass",
            };
            worst(result, &mut verdict);
            checks.push(json!({ "threshold": kind, "limit": limit, "candidate": cand, "result": result }));
        }
        for (kind, limit) in [("max-increase-percent", b.max_increase_percent), ("max-decrease-percent", b.max_decrease_percent)] {
            let Some(limit) = limit else { continue };
            let (result, delta, reason): (&'static str, Option<f64>, String) = match (&comp, base, cand) {
                (None, _, _) => ("incomparable", None, "no baseline is available for a relative threshold".into()),
                // Hardware, toolchain, scenario or cache-state mismatch: no relative verdict (FR-130.3).
                (Some((state, diffs)), _, _) if *state == "incomparable" => (
                    "incomparable",
                    None,
                    format!("baseline and candidate differ in {}", diffs.iter().filter(|d| d["severity"] == json!("blocking")).filter_map(|d| d["what"].as_str()).collect::<Vec<_>>().join(", ")),
                ),
                (_, Some(base), Some(c)) if base > 0.0 => {
                    let raw = (c - base) / base * 100.0;
                    // Movement inside the noise band counts as no change.
                    let delta = if raw.abs() <= budgets.noise_percent { 0.0 } else { raw };
                    let over = if kind == "max-increase-percent" { delta > limit } else { -delta > limit };
                    (if over { "fail" } else { "pass" }, Some((raw * 10.0).round() / 10.0), String::new())
                }
                _ => ("unavailable", None, "the metric is unavailable in the baseline or the candidate".into()),
            };
            worst(result, &mut verdict);
            checks.push(json!({ "threshold": kind, "limit_percent": limit, "baseline": base, "candidate": cand, "delta_percent": delta, "result": result, "reason": if reason.is_empty() { Value::Null } else { json!(reason) } }));
        }
        failed |= verdict == "fail";
        incomparable |= verdict == "incomparable";
        evals.push(json!({ "metric": b.metric, "unit": metric_unit(&b.metric), "scenario": b.scenario, "result": verdict, "checks": checks }));
    }
    let result = if failed {
        if budgets.blocking {
            "fail"
        } else {
            "warn"
        }
    } else if incomparable {
        "incomparable"
    } else {
        "pass"
    };
    json!({
        "result": result,
        "mode": if budgets.blocking { "blocking" } else { "warn" },
        "policy_source": budgets.source,
        "noise_percent": budgets.noise_percent,
        "baseline": baseline.map(|b| json!({ "id": b["id"], "label": b["label"], "name": b["baseline"]["name"] })),
        "comparability": comp.map(|(state, differences)| json!({ "state": state, "differences": differences })),
        "evaluations": evals,
    })
}

/// Process exit status for a budget result, or `None` when the command's own status stands.
pub fn exit_code(result: &Value, budgets: &Budgets) -> Option<i32> {
    match result["result"].as_str() {
        Some("fail") => Some(crate::EXIT_POLICY_FAIL),
        Some("incomparable") if budgets.incomparable_fails => Some(crate::EXIT_INCOMPARABLE),
        _ => None,
    }
}

/// Budget evaluation after an ordinary build when `ci.enforce-on-build` is set (FR-130 entry point).
/// The verdict goes to stderr; Cargo's stdout is never touched (AC-016).
pub fn enforce_on_build(store: &WsStore, cfg: &Config, report: &Value) -> (Option<i32>, Value) {
    let misconfigured = |reason: String| {
        eprintln!("haul: error: ci.enforce-on-build is set but budgets cannot be evaluated: {}", reason);
        (Some(crate::EXIT_HAUL_ERROR), json!({ "result": "error", "reason": reason, "evaluations": [] }))
    };
    let Some(root) = &store.root else { return misconfigured("the command did not run inside a Cargo workspace".into()) };
    let budgets = match load_budgets(root, cfg) {
        Ok(b) => b,
        Err(e) => return misconfigured(e),
    };
    let name = cfg.str("ci.baseline");
    let needs_baseline = budgets.budgets.iter().any(|b| b.max_increase_percent.is_some() || b.max_decrease_percent.is_some());
    let baseline = match load_baseline(root, name) {
        Ok(b) => Some(b),
        Err(e) if needs_baseline => return misconfigured(e),
        Err(_) => None,
    };
    let result = evaluate(&budgets, baseline.as_ref(), &summarize_run(report));
    let verdict = util::vstr(&result, "result");
    if verdict != "pass" {
        eprintln!("haul: budget {}: {}", verdict, crate::render::budget_one_line(&result));
    }
    (exit_code(&result, &budgets), result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(rustc: &str, class: &str, wall: u64) -> Value {
        json!({
            "id": "x", "label": "x", "kind": "run",
            "scenario": { "name": "clean", "cache_state": "cold" },
            "environment": { "rustc_version": rustc, "host": "h", "machine_class": class, "command_family": "build", "profile": "dev", "targets": [], "cargo_version": "c" },
            "metrics": { "wall_time_ns": wall, "critical_path_ns": wall / 2 },
        })
    }

    fn budgets(text: &str) -> Budgets {
        parse_budgets(text, "test").unwrap()
    }

    #[test]
    fn budget_fails_when_over_the_relative_threshold() {
        // FR-130 acceptance: +15% against a +10% clean-build budget in a compatible environment fails.
        let b = budgets("[policy]\nmode = \"blocking\"\n[[budget]]\nmetric = \"wall_time_ns\"\nscenario = \"clean\"\nmax-increase-percent = 10\n");
        let r = evaluate(&b, Some(&summary("1.99", "m", 100_000)), &summary("1.99", "m", 115_000));
        assert_eq!(r["result"], "fail");
        assert_eq!(r["evaluations"][0]["checks"][0]["delta_percent"], 15.0);
        assert_eq!(exit_code(&r, &b), Some(crate::EXIT_POLICY_FAIL));
        let ok = evaluate(&b, Some(&summary("1.99", "m", 100_000)), &summary("1.99", "m", 109_000));
        assert_eq!(ok["result"], "pass");
        assert_eq!(exit_code(&ok, &b), None);
    }

    #[test]
    fn different_hardware_is_incomparable_not_a_regression() {
        // FR-130 acceptance: different hardware classes ⇒ incomparable.
        let b = budgets("[policy]\nmode = \"blocking\"\n[[budget]]\nmetric = \"wall_time_ns\"\nmax-increase-percent = 10\n");
        let r = evaluate(&b, Some(&summary("1.99", "big", 100)), &summary("1.99", "small", 500));
        assert_eq!(r["result"], "incomparable");
        assert!(r["evaluations"][0]["checks"][0]["reason"].as_str().unwrap().contains("hardware class"));
        assert_eq!(exit_code(&r, &b), None, "incomparable only blocks when the policy says so");
        let strict = budgets("[policy]\nincomparable = \"fail\"\n[[budget]]\nmetric = \"wall_time_ns\"\nmax-increase-percent = 10\n");
        assert_eq!(exit_code(&evaluate(&strict, Some(&summary("1.99", "big", 100)), &summary("1.99", "small", 500)), &strict), Some(crate::EXIT_INCOMPARABLE));
    }

    #[test]
    fn cold_and_warm_cache_runs_are_not_compared_blindly() {
        let mut warm = summary("1.99", "m", 10);
        warm["scenario"]["cache_state"] = json!("warm");
        let (state, diffs) = comparability(&summary("1.99", "m", 100), &warm);
        assert_eq!(state, "incomparable");
        assert_eq!(diffs[0]["field"], "scenario.cache_state");
    }

    #[test]
    fn warn_mode_reports_without_blocking_and_absolute_limits_need_no_baseline() {
        let b = budgets("[[budget]]\nmetric = \"wall_time_ns\"\nmax-value = \"1ms\"\n");
        let r = evaluate(&b, None, &summary("1.99", "m", 5_000_000));
        assert_eq!(r["result"], "warn");
        assert_eq!(r["mode"], "warn");
        assert_eq!(exit_code(&r, &b), None);
        assert_eq!(evaluate(&b, None, &summary("1.99", "m", 900_000))["result"], "pass");
    }

    #[test]
    fn noise_band_and_scenario_filter() {
        let b = budgets("[policy]\nmode = \"blocking\"\nnoise-percent = 5\n[[budget]]\nmetric = \"wall_time_ns\"\nmax-increase-percent = 0\n[[budget]]\nmetric = \"critical_path_ns\"\nscenario = \"small-edit\"\nmax-value = 1\n");
        let r = evaluate(&b, Some(&summary("1.99", "m", 100_000)), &summary("1.99", "m", 104_000));
        assert_eq!(r["result"], "pass", "+4% is inside a 5% noise band");
        assert_eq!(r["evaluations"].as_array().unwrap().len(), 1, "the small-edit budget does not apply to a clean scenario");
        assert_eq!(evaluate(&b, Some(&summary("1.99", "m", 100_000)), &summary("1.99", "m", 106_000))["result"], "fail");
    }

    #[test]
    fn budget_files_are_validated_strictly() {
        assert!(parse_budgets("[[budget]]\nmetric = \"wall_tim\"\nmax-value = 1\n", "f").unwrap_err().contains("unknown metric"));
        assert!(parse_budgets("[[budget]]\nmetric = \"wall_time_ns\"\n", "f").unwrap_err().contains("no threshold"));
        assert!(parse_budgets("[[budget]]\nmetric = \"wall_time_ns\"\nmax-valu = 1\n", "f").unwrap_err().contains("unknown key"));
        assert!(parse_budgets("[policy]\nmode = \"strict\"\n", "f").is_err());
        assert_eq!(budgets("[[budget]]\nmetric = \"artifact_allocated_bytes\"\nmax-value = \"2GiB\"\n").budgets[0].max_value, Some((2u64 << 30) as f64));
    }

    #[test]
    fn compiler_difference_is_shown_before_deltas_and_missing_metrics_are_unavailable() {
        // FR-160 acceptance: different rustc versions are stated explicitly.
        let c = compare(&summary("rustc 1.98", "m", 100), &summary("rustc 1.99", "m", 80));
        assert_eq!(c["comparability"]["state"], "incomparable");
        assert_eq!(c["comparability"]["differences"][0]["what"], "compiler version");
        let rows = c["metrics"].as_array().unwrap();
        let wall = rows.iter().find(|r| r["metric"] == "wall_time_ns").unwrap();
        assert_eq!(wall["delta"], -20);
        assert_eq!(wall["delta_percent"], -20.0);
        let rss = rows.iter().find(|r| r["metric"] == "peak_rss_bytes").unwrap();
        assert_eq!(rss["available"], false);
        assert!(rss.get("delta").is_none());
    }

    #[test]
    fn a_run_on_another_toolchain_is_left_out_of_the_sample_group() {
        // FR-020 acceptance: three repetitions, one on a different toolchain ⇒ excluded.
        let (group, excluded) = homogeneous_group(vec![summary("1.99", "m", 10), summary("1.98", "m", 99), summary("1.99", "m", 12)]);
        assert_eq!(group.len(), 2);
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0]["environment"]["rustc_version"], "1.98");
        let s = summarize_group("b1", &json!({ "name": "clean" }), &group);
        assert_eq!(s["metrics"]["wall_time_ns"], 11);
        assert_eq!(s["samples"]["wall_time_ns"], json!([10, 12]));
        assert!(s["metrics"]["peak_rss_bytes"].is_null());
    }
}
