//! Human-readable output: terminal text and a static, accessible HTML report.
//!
//! Empty sections say whether nothing was found or the data was unavailable
//! (UX-006). HTML escapes every repository-controlled string (SEC-019) and
//! presents all data as tables with text status labels (UX-007).

use crate::util::{human_bytes, human_dur, vstr};
use serde_json::Value;
use std::fmt::Write;

fn dur(v: &Value) -> String {
    v.as_u64().map(human_dur).unwrap_or_else(|| "unavailable".into())
}

fn bytes(v: &Value) -> String {
    v.as_u64().map(human_bytes).unwrap_or_else(|| "unavailable".into())
}

fn num(v: &Value) -> String {
    match v {
        Value::Null => "unavailable".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn list(v: &Value) -> String {
    v.as_array().map(|a| a.iter().map(num).collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

fn why(report: &Value, field: &str) -> Option<String> {
    report["unavailable"][field].as_str().map(str::to_string)
}

fn table(out: &mut String, headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line = |cells: Vec<&str>, out: &mut String| {
        let mut s = String::from("  ");
        for (i, c) in cells.iter().enumerate() {
            let pad = widths[i].saturating_sub(c.chars().count());
            s.push_str(c);
            if i + 1 < cells.len() {
                s.push_str(&" ".repeat(pad + 2));
            }
        }
        let _ = writeln!(out, "{}", s.trim_end());
    };
    line(headers.to_vec(), out);
    for r in rows {
        line(r.iter().map(String::as_str).collect(), out);
    }
}

pub const SECTIONS: &[&str] = &["summary", "timing", "rebuilds", "duplication", "scripts", "proc-macros", "codegen", "cache", "storage", "recommendations"];

fn pkg(u: &Value) -> String {
    let v = vstr(u, "version");
    if v.is_empty() {
        vstr(u, "package")
    } else {
        format!("{}@{}", vstr(u, "package"), v)
    }
}

pub fn summary(r: &Value, out: &mut String) {
    let run = &r["run"];
    let m = &r["metrics"];
    let _ = writeln!(out, "Run {}  ({})", vstr(run, "id"), vstr(run, "started_at"));
    let _ = writeln!(out, "  command      {}", list(&run["command"]["argv_redacted"]).replace(", ", " "));
    let status = match (run["result"]["cargo_exit_code"].as_i64(), run["result"]["signal"].as_i64()) {
        (Some(c), _) => format!("exit {}", c),
        (None, Some(s)) => format!("signal {}", s),
        _ => "unknown".into(),
    };
    let _ = writeln!(out, "  result       {} ({})", vstr(run, "state"), status);
    let _ = writeln!(out, "  toolchain    {} / {}", num(&r["toolchain"]["rustc_version"]), num(&r["toolchain"]["cargo_version"]));
    let _ = writeln!(out, "  upstream     {} ({})", vstr(&run["upstream"], "path"), vstr(&run["upstream"], "rule"));
    let _ = writeln!(out, "  wall time    {}", dur(&m["wall_time_ns"]));
    let _ = writeln!(out, "  critical path {}   (sum of unit time {}, parallelism {})", dur(&m["critical_path_ns"]), dur(&m["sum_unit_time_ns"]), num(&m["average_parallelism"]));
    let _ = writeln!(out, "  units        {} compiled, {} fresh, {} observed", num(&m["units_compiled"]), num(&m["units_fresh"]), num(&m["units_observed"]));
    let _ = writeln!(out, "  artifacts    {} allocated", bytes(&m["artifact_allocated_bytes"]));
    if let Some(o) = r["unavailable"].as_object().filter(|o| !o.is_empty()) {
        let _ = writeln!(out, "  unavailable:");
        for (k, v) in o {
            let _ = writeln!(out, "    {}: {}", k, num(v));
        }
    }
}

pub fn timing(r: &Value, out: &mut String) {
    let _ = writeln!(out, "Timing");
    let units: Vec<&Value> = r["units"].as_array().into_iter().flatten().filter(|u| u["duration_ns"].is_u64()).collect();
    if units.is_empty() {
        let _ = writeln!(out, "  {}", why(r, "units").map(|w| format!("Unavailable: {}", w)).unwrap_or_else(|| "Nothing was compiled in this run.".into()));
        return;
    }
    let model = &r["critical_path_model"];
    let _ = writeln!(out, "  critical path: {}  [{}]", dur(&r["metrics"]["critical_path_ns"]), vstr(model, "completeness"));
    for reason in model["partial_reasons"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "    partial: {}", num(reason));
    }
    let rows: Vec<Vec<String>> = r["critical_path"].as_array().into_iter().flatten().map(|u| vec![vstr(u, "package"), vstr(u, "kind"), dur(&u["duration_ns"]), vstr(u, "freshness"), vstr(u, "edge_confidence")]).collect();
    table(out, &["ON CRITICAL PATH", "KIND", "DURATION", "STATE", "EDGE"], &rows);
    // Default time view: critical-path impact first, then cost.
    let mut sorted = units.clone();
    sorted.sort_by_key(|u| (std::cmp::Reverse(u["on_critical_path"] == true), std::cmp::Reverse(u["duration_ns"].as_u64())));
    let rows: Vec<Vec<String>> = sorted
        .iter()
        .take(15)
        .map(|u| vec![pkg(u), vstr(u, "kind"), vstr(u, "mode"), dur(&u["duration_ns"]), if u["on_critical_path"] == true { "yes".into() } else { "no".into() }, vstr(u, "role"), vstr(u, "cacheability")])
        .collect();
    let _ = writeln!(out, "  slowest units ({} of {}):", rows.len(), units.len());
    table(out, &["UNIT", "KIND", "MODE", "DURATION", "CRITICAL", "ROLE", "CACHE"], &rows);
    let _ = writeln!(out, "  peak concurrent compilers: {}   cpu time: {}   peak rss: {}", num(&r["metrics"]["max_concurrent_compilers"]), dur(&r["metrics"]["cpu_time_ns"]), bytes(&r["metrics"]["peak_rss_bytes"]));
}

pub fn rebuilds(r: &Value, out: &mut String) {
    let _ = writeln!(out, "Rebuild reasons");
    let s = &r["rebuilds"]["summary"];
    let units = r["rebuilds"]["units"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if units.is_empty() {
        let _ = writeln!(out, "  {}", why(r, "units").map(|w| format!("Unavailable: {}", w)).unwrap_or_else(|| "Nothing was rebuilt.".into()));
        return;
    }
    let rows: Vec<Vec<String>> = s["by_reason"].as_object().into_iter().flatten().map(|(k, v)| vec![k.clone(), num(&v["units"]), dur(&v["time_ns"])]).collect();
    table(out, &["REASON", "UNITS", "TIME"], &rows);
    let _ = writeln!(out, "  previously known units rebuilt: {} of {} in the index (ratio {})", num(&s["previously_known"]), num(&s["known_units_in_index"]), num(&s["invalidated_ratio"]));
    let rows: Vec<Vec<String>> = units.iter().filter(|u| u["reason"] != "new").take(20).map(|u| vec![pkg(u), vstr(u, "kind"), vstr(u, "reason"), dur(&u["cost_ns"]), vstr(u, "confidence"), vstr(u, "detail")]).collect();
    if rows.is_empty() {
        let _ = writeln!(out, "  All compiled units were seen for the first time; there is nothing to compare against yet.");
    } else {
        table(out, &["UNIT", "KIND", "REASON", "COST", "CONFIDENCE", "DETAIL"], &rows);
    }
}

pub fn duplication(r: &Value, out: &mut String) {
    let _ = writeln!(out, "Duplication");
    let d = &r["duplication"];
    match d["versions"].as_array() {
        None => {
            let _ = writeln!(out, "  Multiple versions: unavailable: {}", why(r, "duplication.versions").or_else(|| why(r, "workspace")).unwrap_or_else(|| "no dependency resolution for this run".into()));
        }
        Some(v) if v.is_empty() => {
            let _ = writeln!(out, "  Multiple versions: none found.");
        }
        Some(v) => {
            for p in v {
                let _ = writeln!(out, "  {} — {} ({})", vstr(p, "name"), vstr(p, "state"), vstr(p, "explanation"));
                for ver in p["versions"].as_array().into_iter().flatten() {
                    let _ = writeln!(out, "    {:<10} compile {:<12} via {}", vstr(ver, "version"), dur(&ver["observed_compile_ns"]), list(&ver["required_by"]).replace(", ", " → "));
                }
            }
        }
    }
    match d["variants"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        [] => {
            let _ = writeln!(out, "  Build variants: none found among observed units.");
        }
        groups => {
            let _ = writeln!(out, "  Build variants (same package built more than once):");
            for g in groups.iter().take(20) {
                let _ = writeln!(out, "    {}@{} [{}] — {}: {}", vstr(g, "package"), vstr(g, "version"), vstr(g, "kind"), vstr(g, "state"), list(&g["causes"]));
                for v in g["variants"].as_array().into_iter().flatten() {
                    let _ = writeln!(out, "      {:<6} {:<8} features [{}]  {}  dependents: {}", vstr(v, "mode"), vstr(v, "role"), list(&v["features"]), dur(&v["dur_ns"]), list(&v["dependents"]));
                }
            }
        }
    }
    let s = &d["summary"];
    let _ = writeln!(out, "  duplicate units: {}   time: {}   bytes: {}", num(&s["duplicate_units"]), dur(&s["duplicate_time_ns"]), bytes(&s["duplicate_bytes"]));
}

pub fn scripts(r: &Value, out: &mut String) {
    let _ = writeln!(out, "Build scripts");
    let list_ = r["build_scripts"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if list_.is_empty() {
        let _ = writeln!(out, "  {}", why(r, "units").map(|w| format!("Unavailable: {}", w)).unwrap_or_else(|| "No build script was involved in the units compiled by this run.".into()));
        return;
    }
    let rows: Vec<Vec<String>> = list_
        .iter()
        .map(|s| {
            vec![
                pkg(s),
                if s["ran_in_session"] == true { "ran".into() } else { "not run".into() },
                dur(&s["duration_ns"]),
                dur(&s["compile_ns"]),
                if s["has_rerun_directives"] == true { format!("{} file, {} env", num(&s["rerun_if_changed"]), s["rerun_if_env_changed"].as_array().map(Vec::len).unwrap_or(0)) } else { "none".into() },
                bytes(&s["generated_output_bytes"]),
                num(&s["history"]["runs"]),
                vstr(s, "state"),
            ]
        })
        .collect();
    table(out, &["PACKAGE", "THIS RUN", "RUN TIME", "COMPILE", "RERUN-IF", "OUT_DIR", "RUNS SEEN", "STATE"], &rows);
    if !list_.iter().any(|s| s["state"] == "over-invalidating-candidate") {
        let _ = writeln!(out, "  No broad invalidation detected.");
    }
}

pub fn proc_macros(r: &Value, out: &mut String) {
    let _ = writeln!(out, "Procedural macros");
    let units = r["proc_macros"]["units"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if units.is_empty() {
        let _ = writeln!(out, "  {}", why(r, "units").map(|w| format!("Unavailable: {}", w)).unwrap_or_else(|| "No proc-macro crate was compiled or linked against in this run.".into()));
    } else {
        let rows: Vec<Vec<String>> = units.iter().map(|u| vec![pkg(u), vstr(u, "freshness"), dur(&u["compile_ns"]), num(&u["direct_dependents"]), num(&u["downstream_units"]), dur(&u["downstream_compile_ns"]), vstr(u, "state")]).collect();
        table(out, &["MACRO CRATE", "STATE", "OWN COMPILE", "DIRECT USERS", "DOWNSTREAM", "DOWNSTREAM TIME", "CLASS"], &rows);
    }
    let _ = writeln!(out, "  Proc-macro execution timing\n    Unavailable: {}", num(&r["proc_macros"]["unavailable"]["execution_time_ns"]));
}

pub fn codegen(r: &Value, out: &mut String) {
    let _ = writeln!(out, "Codegen and linking");
    let settings = r["codegen"]["settings"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if settings.is_empty() {
        let _ = writeln!(out, "  {}", why(r, "units").map(|w| format!("Unavailable: {}", w)).unwrap_or_else(|| "Nothing was compiled in this run.".into()));
        return;
    }
    let rows: Vec<Vec<String>> = settings
        .iter()
        .map(|s| {
            let g = &s["settings"];
            vec![vstr(g, "class"), vstr(g, "role"), num(&g["opt_level"]), num(&g["debuginfo"]), g["codegen_units"].as_str().unwrap_or("default").into(), g["lto"].as_str().unwrap_or("default").into(), if g["incremental"] == true { "on".into() } else { "off".into() }, num(&s["units"]), dur(&s["time_ns"])]
        })
        .collect();
    table(out, &["CLASS", "ROLE", "OPT", "DEBUG", "CGU", "LTO", "INCR", "UNITS", "TIME"], &rows);
    let links = r["codegen"]["linking"]["units"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if links.is_empty() {
        let _ = writeln!(out, "  No linking unit in this run.");
    } else {
        let rows: Vec<Vec<String>> = links.iter().take(10).map(|l| vec![vstr(l, "package"), vstr(l, "kind"), dur(&l["duration_ns"]), vstr(l, "linker"), bytes(&l["output_bytes"])]).collect();
        table(out, &["LINKING UNIT", "KIND", "UNIT TIME", "LINKER", "OUTPUT"], &rows);
        let _ = writeln!(out, "  Link phase timing\n    Unavailable: {}", num(&r["codegen"]["linking"]["unavailable"]["link_phase_ns"]));
    }
    let inc = &r["policy"]["incremental"];
    let _ = writeln!(out, "  incremental: {} — {}", vstr(inc, "decision"), vstr(inc, "rationale"));
    let _ = writeln!(out, "    {}", vstr(inc, "explanation"));
}

pub fn cache(r: &Value, out: &mut String) {
    let _ = writeln!(out, "Compiler cache");
    let c = &r["cache"];
    if c["hits"].is_null() {
        let _ = writeln!(out, "  {}: {}", num(&c["state"]), c["unavailable"].as_str().or(c["detail"].as_str()).unwrap_or("no statistics"));
        return;
    }
    let (h, m) = (c["hits"].as_u64().unwrap_or(0), c["misses"].as_u64().unwrap_or(0));
    // The rate is always shown with its request count (FR-100.1).
    let _ = writeln!(out, "  sccache {}  [{}]", num(&c["version"]), num(&c["state"]));
    let _ = writeln!(out, "  hits            {}/{} cacheable requests{}", h, h + m, if h + m > 0 { format!(" ({:.0}%)", h as f64 * 100.0 / (h + m) as f64) } else { String::new() });
    let _ = writeln!(out, "  misses          {}", m);
    let _ = writeln!(out, "  non-cacheable   {}  {}", num(&c["non_cacheable"]), c["non_cacheable_reasons"].as_object().filter(|o| !o.is_empty()).map(|o| format!("({})", o.iter().map(|(k, v)| format!("{}: {}", k, v)).collect::<Vec<_>>().join(", "))).unwrap_or_default());
    let _ = writeln!(out, "  compile requests {}   errors {}   timeouts {}", num(&c["requests"]), num(&c["errors"]), num(&c["timeouts"]));
    let _ = writeln!(out, "  restore latency {}   write latency {}", dur(&c["lookup_time_ns"]), dur(&c["write_time_ns"]));
    let _ = writeln!(out, "  bytes restored  {}   bytes written {}", bytes(&c["read_bytes"]), bytes(&c["write_bytes"]));
    let _ = writeln!(out, "  units by rule   {} cacheable, {} linking, {} incremental", num(&c["units"]["cacheable"]), num(&c["units"]["non_cacheable_linking"]), num(&c["units"]["non_cacheable_incremental"]));
    let _ = writeln!(out, "  scope: {}", vstr(c, "scope"));
}

pub fn inventory(inv: &Value, out: &mut String) {
    let _ = writeln!(out, "  {}  — {} allocated, {} logical, {} files{}", vstr(inv, "root"), bytes(&inv["allocated_bytes"]), bytes(&inv["logical_bytes"]), num(&inv["files"]), if inv["complete"] == true { "" } else { "  [partial: some entries could not be read]" });
    // Default storage view: allocated bytes, largest first.
    let mut rows: Vec<(u64, Vec<String>)> = inv["categories"].as_object().into_iter().flatten().map(|(k, v)| (v["allocated_bytes"].as_u64().unwrap_or(0), vec![k.clone(), bytes(&v["allocated_bytes"]), bytes(&v["logical_bytes"]), num(&v["files"])])).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    table(out, &["CATEGORY", "ALLOCATED", "LOGICAL", "FILES"], &rows.into_iter().map(|r| r.1).collect::<Vec<_>>());
    let rows: Vec<Vec<String>> = inv["reclaimability"].as_object().into_iter().flatten().map(|(k, v)| vec![k.clone(), bytes(&v["allocated_bytes"]), num(&v["files"])]).collect();
    table(out, &["RECLAIMABILITY", "ALLOCATED", "FILES"], &rows);
    if inv["hardlink_duplicate_bytes_excluded"].as_u64().unwrap_or(0) > 0 {
        let _ = writeln!(out, "  hard-linked copies counted once: {} excluded", bytes(&inv["hardlink_duplicate_bytes_excluded"]));
    }
    for e in inv["errors"]["samples"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "  unreadable: {}", num(e));
    }
}

pub fn storage(r: &Value, out: &mut String) {
    let _ = writeln!(out, "Storage");
    match r["artifacts"].as_array() {
        Some(a) if !a.is_empty() => a.iter().for_each(|inv| inventory(inv, out)),
        _ => {
            let _ = writeln!(out, "  Unavailable: {}", why(r, "artifacts").unwrap_or_else(|| "no inventory was taken".into()));
        }
    }
}

pub fn gc_plan(inv: &Value, out: &mut String) {
    let p = &inv["gc_plan"];
    let _ = writeln!(out, "GC plan (dry run) for {}", vstr(inv, "root"));
    let _ = writeln!(out, "  scope: rebuildable output unused for more than {} days", num(&p["stale_days"]));
    let _ = writeln!(out, "  estimated reclaimable: {} in {} group(s)", bytes(&p["candidate_bytes"]), num(&p["candidate_groups"]));
    if let Some(b) = p["blocked_by"].as_str() {
        let _ = writeln!(out, "  BLOCKED: {} (reclaimable now: {})", b, bytes(&p["reclaimable_now_bytes"]));
    }
    let regen = &p["regeneration_estimate"];
    let _ = writeln!(out, "  cost to regenerate: {} ({} unit(s) with measured cost, {} unknown)", dur(&regen["compile_time_ns"]), num(&regen["units_with_known_cost"]), num(&regen["units_with_unknown_cost"]));
    if let Some(b) = p["size_budget"].as_object() {
        let _ = writeln!(out, "  size budget {}: currently {}, {} after the plan — {}", bytes(&b["target_bytes"]), bytes(&b["current_bytes"]), bytes(&b["after_plan_bytes"]), if b["met_by_plan"] == true { "met" } else { "NOT met by stale output alone" });
    }
    let rows: Vec<Vec<String>> = p["candidates"].as_array().into_iter().flatten().take(25).map(|c| vec![vstr(c, "group"), vstr(c, "category"), bytes(&c["allocated_bytes"]), format!("{}d", num(&c["age_days"])), vstr(c, "reason")]).collect();
    if rows.is_empty() {
        let _ = writeln!(out, "  No candidates: nothing is both rebuildable and stale.");
    } else {
        table(out, &["CANDIDATE", "CATEGORY", "ALLOCATED", "AGE", "WHY"], &rows);
    }
    let _ = writeln!(out, "  Nothing was deleted. This release plans only; `cargo clean` is unchanged.");
}

pub fn recommendations(recs: &Value, out: &mut String) {
    let _ = writeln!(out, "Recommendations");
    let recs = recs.as_array().map(Vec::as_slice).unwrap_or(&[]);
    if recs.is_empty() {
        let _ = writeln!(out, "  None: the recorded evidence supports no recommendation.");
        return;
    }
    for (i, r) in recs.iter().enumerate() {
        let impact = &r["impact"];
        let gain = match impact["kind"].as_str() {
            Some(k @ ("measured" | "modeled")) => {
                let mut parts = Vec::new();
                if let Some(t) = impact["avoidable_time_ns"].as_u64() {
                    parts.push(format!("{} avoidable", human_dur(t)));
                }
                if let Some(b) = impact["reclaimable_bytes"].as_u64() {
                    parts.push(format!("{} reclaimable", human_bytes(b)));
                }
                format!("{}: {}", k, parts.join(", "))
            }
            _ => "gain unknown (not measured)".into(),
        };
        let _ = writeln!(out, "  {}. {}", i + 1, vstr(r, "title"));
        let _ = writeln!(out, "     id {}   [{}]   confidence {} · risk {} · effort {} · {}", vstr(r, "id"), vstr(r, "category"), vstr(r, "confidence"), vstr(r, "risk"), vstr(r, "effort"), gain);
        for e in r["evidence"].as_array().into_iter().flatten() {
            let _ = writeln!(out, "     - {}", num(e));
        }
        if let Some(alts) = r["alternatives"].as_array() {
            let _ = writeln!(out, "     alternatives (choose one):");
            for a in alts {
                let _ = writeln!(out, "       * {}: {} — suits {}", vstr(a, "id"), vstr(a, "action"), vstr(a, "suits"));
            }
        }
        let _ = writeln!(out, "     verify: {}", vstr(r, "verification"));
        let _ = writeln!(out, "     constraints: {}", vstr(r, "compatibility"));
        let _ = writeln!(out, "     remediation: {}", vstr(r, "remediation"));
    }
}

pub fn run_text(r: &Value, sections: &[&str]) -> String {
    let mut out = String::new();
    for (i, s) in sections.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        match *s {
            "summary" => summary(r, &mut out),
            "timing" => timing(r, &mut out),
            "rebuilds" => rebuilds(r, &mut out),
            "duplication" => duplication(r, &mut out),
            "scripts" => scripts(r, &mut out),
            "proc-macros" => proc_macros(r, &mut out),
            "codegen" => codegen(r, &mut out),
            "cache" => cache(r, &mut out),
            "storage" => storage(r, &mut out),
            "recommendations" => recommendations(&r["recommendations"], &mut out),
            _ => {}
        }
    }
    out
}

fn metric_value(v: &Value, unit: &str) -> String {
    match (v.as_f64(), unit) {
        (None, _) => "unavailable".into(),
        (Some(x), "ns") => human_dur(x as u64),
        (Some(x), "bytes") => human_bytes(x as u64),
        (Some(x), "ratio") => format!("{:.0}%", x * 100.0),
        (Some(x), _) => format!("{}", x),
    }
}

pub fn comparison(c: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Comparison: {}  →  {}", vstr(&c["baseline"], "label"), vstr(&c["candidate"], "label"));
    // Environment differences come before any number (FR-160.3).
    let state = vstr(&c["comparability"], "state");
    let _ = writeln!(out, "  comparability: {}", state);
    for d in c["comparability"]["differences"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "    [{}] {}: {} → {}", vstr(d, "severity"), vstr(d, "what"), num(&d["baseline"]), num(&d["candidate"]));
    }
    if state == "incomparable" {
        let _ = writeln!(out, "  The deltas below describe different conditions and must not be read as a speed-up or regression.");
    }
    let rows: Vec<Vec<String>> = c["metrics"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| {
            let unit = vstr(m, "unit");
            let delta = match (m["available"] == true, m["delta_percent"].as_f64()) {
                (true, Some(p)) => format!("{:+.1}%", p),
                (true, None) => "n/a".into(),
                (false, _) => format!("unavailable ({})", vstr(m, "unavailable_in")),
            };
            vec![vstr(m, "metric"), metric_value(&m["baseline"], &unit), metric_value(&m["candidate"], &unit), delta]
        })
        .collect();
    table(&mut out, &["METRIC", "BASELINE", "CANDIDATE", "DELTA"], &rows);
    out
}

pub fn budget_one_line(b: &Value) -> String {
    let failing: Vec<String> = b["evaluations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["result"] != "pass")
        .map(|e| {
            let check = e["checks"].as_array().and_then(|c| c.iter().find(|c| c["result"] != "pass")).cloned().unwrap_or(Value::Null);
            match check["delta_percent"].as_f64() {
                Some(d) => format!("{} {:+.1}% (limit {}%)", vstr(e, "metric"), d, num(&check["limit_percent"])),
                None => format!("{} {}", vstr(e, "metric"), check["reason"].as_str().map(str::to_string).unwrap_or_else(|| vstr(e, "result"))),
            }
        })
        .collect();
    if failing.is_empty() {
        vstr(b, "result")
    } else {
        failing.join("; ")
    }
}

pub fn budget(b: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Budget result: {}  (mode {}, policy {})", vstr(b, "result").to_uppercase(), vstr(b, "mode"), vstr(b, "policy_source"));
    match b["baseline"].as_object() {
        Some(base) => {
            let _ = writeln!(out, "  baseline: {}   comparability: {}", base["label"].as_str().unwrap_or(""), vstr(&b["comparability"], "state"));
        }
        None => {
            let _ = writeln!(out, "  baseline: none");
        }
    }
    for d in b["comparability"]["differences"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "    [{}] {}: {} → {}", vstr(d, "severity"), vstr(d, "what"), num(&d["baseline"]), num(&d["candidate"]));
    }
    let mut rows = Vec::new();
    for e in b["evaluations"].as_array().into_iter().flatten() {
        let unit = vstr(e, "unit");
        for c in e["checks"].as_array().into_iter().flatten() {
            let limit = match c["limit_percent"].as_f64() {
                Some(p) => format!("{} {}%", vstr(c, "threshold"), p),
                None => format!("{} {}", vstr(c, "threshold"), metric_value(&c["limit"], &unit)),
            };
            let observed = match c["delta_percent"].as_f64() {
                Some(d) => format!("{} ({:+.1}%)", metric_value(&c["candidate"], &unit), d),
                None => metric_value(&c["candidate"], &unit),
            };
            rows.push(vec![vstr(e, "metric"), e["scenario"].as_str().unwrap_or("any").into(), limit, observed, vstr(c, "result").to_uppercase(), c["reason"].as_str().unwrap_or("").into()]);
        }
    }
    if rows.is_empty() {
        let _ = writeln!(out, "  No budget applies to this scenario.");
    } else {
        table(&mut out, &["METRIC", "SCENARIO", "THRESHOLD", "OBSERVED", "RESULT", "NOTE"], &rows);
    }
    out
}

pub fn benchmark(b: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Benchmark {}  scenario {} ({})  — {}", vstr(b, "id"), vstr(&b["scenario"], "name"), vstr(&b["scenario"], "cache_state"), vstr(b, "state").to_uppercase());
    if let Some(f) = b["failure"].as_str() {
        let _ = writeln!(out, "  failure: {}", f);
    }
    let _ = writeln!(out, "  command: {}", list(&b["command"]).replace(", ", " "));
    let rows: Vec<Vec<String>> = b["runs"].as_array().into_iter().flatten().map(|r| vec![vstr(r, "phase"), num(&r["iteration"]), dur(&r["wall_time_ns"]), if r["valid"] == true { "valid".into() } else { format!("INVALID: {}", vstr(r, "invalid_reason")) }, r["run_id"].as_str().unwrap_or("").into()]).collect();
    table(&mut out, &["PHASE", "#", "WALL", "SAMPLE", "RUN"], &rows);
    let d = &b["summary"]["distributions"]["wall_time_ns"];
    if d.is_object() {
        let _ = writeln!(out, "  wall time: median {}  (min {}, max {}, stddev {}, n={})", dur(&d["median"]), dur(&d["min"]), dur(&d["max"]), dur(&d["stddev"]), num(&d["n"]));
        let _ = writeln!(out, "  critical path (median): {}   units compiled (median): {}", dur(&b["summary"]["metrics"]["critical_path_ns"]), num(&b["summary"]["metrics"]["units_compiled"]));
    }
    let _ = writeln!(out, "  samples: {} valid, {} discarded", num(&b["samples_valid"]), num(&b["samples_discarded"]));
    if let Some(m) = b["mutation"].as_object() {
        let _ = writeln!(out, "  synthetic edit: {} — {}", m["file"].as_str().unwrap_or(""), if m["restored"] == true { "restored" } else { "NOT RESTORED" });
    }
    out
}

pub fn experiment(e: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Experiment `{}` — {}", vstr(e, "name"), vstr(e, "status").to_uppercase());
    let _ = writeln!(out, "  {}", vstr(e, "reason"));
    let _ = writeln!(out, "  candidate settings: {} {}", list(&e["candidate_settings"]["cargo_config"]), list(&e["candidate_settings"]["environment"]));
    let _ = writeln!(out, "  {}", vstr(&e["candidate_settings"], "isolation"));
    let b = &e["dimensions"]["build"];
    let _ = writeln!(out, "  build time: {}  (run-to-run noise ±{:.1}%)", b["wall_time_delta_percent"].as_f64().map(|d| format!("{:+.1}%", d)).unwrap_or_else(|| "unavailable".into()), b["noise_percent"].as_f64().unwrap_or(0.0));
    let _ = writeln!(out, "  runtime and debugging:");
    for r in e["dimensions"]["runtime_and_debugging"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "    {}: {}", vstr(r, "setting"), vstr(r, "risk"));
    }
    let v = &e["verification"];
    let _ = writeln!(out, "  verification: {}", if v["performed"] == true { if v["passed"] == true { "passed".to_string() } else { format!("FAILED (exit {})", num(&v["exit_code"])) } } else { format!("not performed ({})", vstr(v, "reason")) });
    out.push_str(&comparison(&e["comparison"]));
    out
}

pub fn worktrees(w: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Worktrees and other workspaces");
    let _ = writeln!(out, "  this workspace: {}  (build directory {})", vstr(w, "workspace"), num(&w["build_directory"]));
    if let Some(u) = w["unavailable"]["worktrees"].as_str() {
        let _ = writeln!(out, "  Unavailable: {}", u);
    }
    let others = w["others"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if others.is_empty() {
        let _ = writeln!(out, "  No other worktree or recorded workspace found.");
        return out;
    }
    let rows: Vec<Vec<String>> = others.iter().map(|o| vec![vstr(o, "root"), match o["same_repository"].as_bool() { Some(true) => "yes".into(), Some(false) => "no".into(), None => "unknown".into() }, vstr(o, "state"), num(&o["identical_units"]), dur(&o["identical_unit_compile_ns"]), bytes(&o["identical_unit_bytes"])]).collect();
    table(&mut out, &["WORKSPACE", "SAME REPO", "STATE", "IDENTICAL UNITS", "COMPILE TIME", "BYTES"], &rows);
    let s = &w["summary"];
    let _ = writeln!(out, "  potential cross-worktree reuse: {} unit(s), {} of compile time, {} stored twice", num(&s["duplicate_units"]), dur(&s["duplicate_compile_ns"]), bytes(&s["duplicate_bytes"]));
    for a in w["advice"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "  - {}", num(a));
    }
    out
}

pub fn remediations(list_: &[Value], patch_only: bool) -> String {
    let mut out = String::new();
    for r in list_ {
        if patch_only {
            if let Some(d) = r["diff"].as_str() {
                out.push_str(d);
            }
            continue;
        }
        let _ = writeln!(out, "Remediation for {}  [{} · {}]", vstr(r, "recommendation"), vstr(r, "class"), vstr(r, "state"));
        let _ = writeln!(out, "  {}", vstr(r, "summary"));
        if let Some(d) = r["diff"].as_str() {
            let _ = writeln!(out, "  id: {}   file: {}   preimage: {}", vstr(r, "id"), vstr(r, "file"), r["preimage_sha256"].as_str().map(|s| &s[..12]).unwrap_or("(file does not exist)"));
            for l in d.lines() {
                let _ = writeln!(out, "    {}", l);
            }
            let _ = writeln!(out, "  {}", vstr(r, "apply_note"));
        }
        let _ = writeln!(out, "  verify: {}\n", vstr(r, "verification"));
    }
    out
}

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

fn html_table(out: &mut String, caption: &str, headers: &[&str], rows: &[Vec<String>], empty: &str) {
    let _ = write!(out, "<table><caption>{}</caption><thead><tr>", esc(caption));
    for h in headers {
        let _ = write!(out, "<th scope=\"col\">{}</th>", esc(h));
    }
    out.push_str("</tr></thead><tbody>");
    if rows.is_empty() {
        let _ = write!(out, "<tr><td colspan=\"{}\">{}</td></tr>", headers.len(), esc(empty));
    }
    for r in rows {
        out.push_str("<tr>");
        for (i, c) in r.iter().enumerate() {
            if i == 0 {
                let _ = write!(out, "<th scope=\"row\">{}</th>", esc(c));
            } else {
                let _ = write!(out, "<td>{}</td>", esc(c));
            }
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>\n");
}

/// Self-contained HTML report: no scripts, no external resources, tables for every data set.
pub fn html(r: &Value) -> String {
    let mut o = String::new();
    let id = vstr(&r["run"], "id");
    let _ = write!(
        o,
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Haul run {}</title>\n<style>\
body{{font:16px/1.5 system-ui,sans-serif;margin:2rem auto;max-width:72rem;padding:0 1rem;color:#1a1a1a;background:#fff}}\
h1,h2{{line-height:1.2}}table{{border-collapse:collapse;width:100%;margin:1rem 0 2rem}}caption{{text-align:left;font-weight:600;padding:.25rem 0}}\
th,td{{border:1px solid #767676;padding:.3rem .6rem;text-align:left;vertical-align:top}}thead th{{background:#e8e8e8}}\
tbody th{{font-weight:600}}pre{{white-space:pre-wrap;background:#f3f3f3;padding:1rem;border:1px solid #767676}}\
@media (prefers-color-scheme:dark){{body{{background:#121212;color:#f0f0f0}}thead th,pre{{background:#2a2a2a}}th,td,pre{{border-color:#9a9a9a}}}}\
</style></head><body><main>\n<h1>Haul run {}</h1>\n",
        esc(&id),
        esc(&id)
    );
    let mut text = String::new();
    summary(r, &mut text);
    let _ = writeln!(o, "<h2>Summary</h2><pre>{}</pre>", esc(&text));

    o.push_str("<h2>Timing</h2>\n");
    let rows: Vec<Vec<String>> = r["critical_path"].as_array().into_iter().flatten().map(|u| vec![vstr(u, "package"), vstr(u, "kind"), dur(&u["duration_ns"]), vstr(u, "freshness"), vstr(u, "edge_confidence")]).collect();
    html_table(&mut o, &format!("Critical path ({}, model: {})", dur(&r["metrics"]["critical_path_ns"]), vstr(&r["critical_path_model"], "completeness")), &["Unit", "Kind", "Duration", "State", "Edge confidence"], &rows, &why(r, "units").map(|w| format!("Unavailable: {}", w)).unwrap_or_else(|| "Nothing was compiled.".into()));
    let mut units: Vec<&Value> = r["units"].as_array().into_iter().flatten().filter(|u| u["duration_ns"].is_u64()).collect();
    units.sort_by_key(|u| (std::cmp::Reverse(u["on_critical_path"] == true), std::cmp::Reverse(u["duration_ns"].as_u64())));
    let rows: Vec<Vec<String>> = units.iter().map(|u| vec![pkg(u), vstr(u, "kind"), vstr(u, "mode"), dur(&u["duration_ns"]), if u["on_critical_path"] == true { "yes".into() } else { "no".into() }, vstr(u, "role"), list(&u["features"]), vstr(u, "cacheability")]).collect();
    html_table(&mut o, "Units, critical-path impact first", &["Unit", "Kind", "Mode", "Duration", "On critical path", "Role", "Features", "Cache disposition"], &rows, "No unit timings.");

    o.push_str("<h2>Rebuild reasons</h2>\n");
    let rows: Vec<Vec<String>> = r["rebuilds"]["units"].as_array().into_iter().flatten().map(|u| vec![pkg(u), vstr(u, "reason"), dur(&u["cost_ns"]), vstr(u, "confidence"), vstr(u, "detail"), list(&u["evidence"])]).collect();
    html_table(&mut o, "Why each unit was compiled", &["Unit", "Reason", "Cost", "Confidence", "Detail", "Evidence"], &rows, "Nothing was rebuilt.");

    for (title, section) in [("Duplication", "duplication"), ("Build scripts", "scripts"), ("Procedural macros", "proc-macros"), ("Codegen and linking", "codegen"), ("Compiler cache", "cache"), ("Storage", "storage")] {
        let _ = writeln!(o, "<h2>{}</h2><pre>{}</pre>", title, esc(&run_text(r, &[section])));
    }

    o.push_str("<h2>Recommendations</h2>\n");
    let rows: Vec<Vec<String>> = r["recommendations"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|x| {
            let impact = &x["impact"];
            let gain = match (impact["kind"].as_str(), impact["avoidable_time_ns"].as_u64(), impact["reclaimable_bytes"].as_u64()) {
                (Some("unknown") | None, _, _) => "unknown (not measured)".to_string(),
                (Some(k), Some(t), _) => format!("{}: {}", k, human_dur(t)),
                (Some(k), None, Some(b)) => format!("{}: {}", k, human_bytes(b)),
                (Some(k), None, None) => k.to_string(),
            };
            vec![vstr(x, "title"), vstr(x, "category"), gain, vstr(x, "confidence"), vstr(x, "risk"), vstr(x, "effort"), list(&x["evidence"]), vstr(x, "verification"), vstr(x, "remediation")]
        })
        .collect();
    html_table(&mut o, "Ranked recommendations", &["Recommendation", "Category", "Expected gain", "Confidence", "Risk", "Effort", "Evidence", "Verification", "Remediation"], &rows, "None: the recorded evidence supports no recommendation.");
    o.push_str("</main></body></html>\n");
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn html_escapes_repository_controlled_strings() {
        // SEC-019: a hostile package name must not become markup.
        let r = json!({
            "run": { "id": "X", "command": { "argv_redacted": ["cargo", "build"] }, "result": {}, "upstream": {} },
            "metrics": {}, "toolchain": {}, "unavailable": {}, "critical_path_model": {},
            "critical_path": [{ "package": "<script>alert(1)</script>", "kind": "lib", "duration_ns": 5, "freshness": "compiled", "edge_confidence": "observed" }],
            "units": [{ "package": "a\"><img src=x onerror=y>", "version": "1", "kind": "lib", "mode": "build", "duration_ns": 5, "features": ["<b>"] }],
            "rebuilds": {}, "recommendations": [{ "title": "</td><script>", "impact": { "kind": "unknown" }, "evidence": ["&<>"] }],
        });
        let h = html(&r);
        assert!(!h.contains("<script>"), "raw script tag leaked into the report");
        assert!(!h.contains("<img"));
        assert!(h.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(h.contains("a&quot;&gt;&lt;img src=x onerror=y&gt;@1"));
    }

    #[test]
    fn empty_sections_distinguish_nothing_found_from_unavailable() {
        // UX-006.
        let none = json!({ "build_scripts": [], "unavailable": {} });
        assert!(run_text(&none, &["scripts"]).contains("No build script was involved"));
        let unavailable = json!({ "build_scripts": [], "unavailable": { "units": "rustc-level observation was not active for this session" } });
        assert!(run_text(&unavailable, &["scripts"]).contains("Unavailable: rustc-level observation was not active"));
        let pm = json!({ "proc_macros": { "units": [], "unavailable": { "execution_time_ns": "not exposed" } }, "unavailable": {} });
        let text = run_text(&pm, &["proc-macros"]);
        assert!(text.contains("No proc-macro crate") && text.contains("Unavailable: not exposed"));
    }

    #[test]
    fn hit_rate_is_never_shown_without_its_request_count() {
        let r = json!({ "cache": { "hits": 80, "misses": 20, "non_cacheable": 3, "requests": 110, "state": "local", "non_cacheable_reasons": {}, "units": {} } });
        assert!(run_text(&r, &["cache"]).contains("80/100 cacheable requests (80%)"));
    }

    #[test]
    fn unknown_gains_are_not_given_a_number() {
        let mut out = String::new();
        recommendations(&json!([{ "title": "t", "impact": { "kind": "unknown" }, "evidence": [] }]), &mut out);
        assert!(out.contains("gain unknown (not measured)"));
    }
}
