//! Waste analysis over the observed build model: why units rebuilt (FR-040),
//! what is built more than once (FR-050), build scripts (FR-060), proc macros
//! (FR-070), codegen/linker visibility (FR-080), incremental policy (FR-090)
//! and cross-worktree duplication (FR-120).
//!
//! Every conclusion names its evidence. Where the evidence does not decide,
//! the answer is `unknown`, which is a valid final classification (FR-040.2).

use crate::analyze::{CriticalPath, Graph, Unit};
use crate::discover::Metadata;
use crate::inventory::Knowledge;
use crate::store::{self, WsStore};
use crate::util::{self, path_str};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

const VARIANTS_KEPT: usize = 8;
const REBUILD_LOG_KEPT: usize = 12;

/// Last known fingerprints of every unit QuickRust has seen in a workspace.
pub struct Index {
    v: Value,
}

impl Index {
    pub fn empty() -> Index {
        Index { v: json!({ "version": 1, "last_clean_wall_ns": null, "units": {}, "rebuild_log": {}, "build_scripts": {} }) }
    }

    /// A missing index is an empty one; an unreadable one is an error to be fixed, not papered over.
    pub fn load(path: &Path) -> Result<Index, String> {
        if !path.exists() {
            return Ok(Index::empty());
        }
        let v = util::read_json(path).filter(|v| v["version"] == json!(1) && v["units"].is_object()).ok_or_else(|| {
            format!("the unit index {} is unreadable or of an unknown version; remove it to rebuild it from future runs", path.display())
        })?;
        Ok(Index { v })
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(&self.v).map_err(std::io::Error::other)?;
        util::write_atomic(path, &bytes)
    }

    pub fn entries(&self, logical_key: &str) -> &[Value] {
        self.v["units"][logical_key].as_array().map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn all(&self) -> impl Iterator<Item = &Value> {
        self.v["units"].as_object().into_iter().flat_map(|o| o.values()).filter_map(Value::as_array).flatten()
    }

    /// Every entry by its artifact stem, borrowed: an index of ten thousand units is large.
    pub fn by_stem(&self) -> HashMap<&str, &Value> {
        self.all().filter_map(|e| Some((e["stem"].as_str()?, e))).collect()
    }

    pub fn last_clean(&self) -> Option<u64> {
        self.v["last_clean_wall_ns"].as_u64()
    }

    pub fn rebuild_log(&self) -> &Value {
        &self.v["rebuild_log"]
    }

    pub fn build_script_history(&self, key: &str) -> &Value {
        &self.v["build_scripts"][key]
    }

    /// What the inventory needs: liveness, proc-macro identity and regeneration cost per artifact.
    pub fn knowledge(&self, graph: &Graph) -> Knowledge {
        let mut k = Knowledge::default();
        for e in self.all() {
            let Some(stem) = e["stem"].as_str() else { continue };
            k.last_used_ns.insert(stem.to_string(), e["last_used_ns"].as_u64().unwrap_or(0));
            if let Some(d) = e["dur_ns"].as_u64() {
                k.cost_ns.insert(stem.to_string(), d);
            }
            if e["kind"] == json!("proc-macro") {
                k.proc_macro_stems.insert(stem.to_string());
            }
        }
        let now = util::now_wall_ns();
        for u in &graph.units {
            k.last_used_ns.insert(u.stem.clone(), now);
            if let Some(d) = u.duration_ns {
                k.cost_ns.insert(u.stem.clone(), d);
            }
            if u.kind == "proc-macro" {
                k.proc_macro_stems.insert(u.stem.clone());
            }
        }
        k
    }

    /// Records the outcome of a finalized run.
    pub fn absorb(&mut self, graph: &Graph, rebuilds: &Value, run: &str, wall_ns: u64, family: &str, ok: bool) {
        if family == "clean" && ok {
            self.v["last_clean_wall_ns"] = json!(wall_ns);
        }
        let units = self.v["units"].as_object_mut().expect("index shape is validated on load");
        // Fresh units were linked against: refresh their liveness.
        let fresh: HashSet<&str> = graph.units.iter().filter(|u| u.freshness == "fresh").map(|u| u.stem.as_str()).collect();
        if !fresh.is_empty() {
            for list in units.values_mut().filter_map(Value::as_array_mut) {
                for e in list.iter_mut() {
                    if e["stem"].as_str().map(|s| fresh.contains(s)).unwrap_or(false) {
                        e["last_used_ns"] = json!(wall_ns);
                    }
                }
            }
        }
        for u in graph.units.iter().filter(|u| u.built() && u.kind != "build-script-run") {
            let entry = json!({
                "exact_id": u.exact_id, "portable_id": u.portable_id, "stem": u.stem, "out_dir": u.out_dir, "run": run,
                "wall_ns": wall_ns, "last_used_ns": wall_ns, "dur_ns": u.duration_ns, "bytes": u.output_bytes,
                "package": u.package, "version": u.version, "source": u.source, "manifest_dir": u.manifest_dir,
                "crate_name": u.crate_name, "kind": u.kind, "mode": u.mode, "role": u.role, "target": u.target,
                "features": u.features, "codegen": u.codegen, "incremental": u.incremental, "fingerprint": u.fingerprint,
                "inputs": u.inputs.iter().map(|(p, (d, m))| (p.clone(), json!([d, m]))).collect::<Map<_, _>>(),
            });
            let list = units.entry(u.logical_key.clone()).or_insert_with(|| json!([]));
            let list = list.as_array_mut().expect("index shape is validated on load");
            list.retain(|e| e["stem"] != json!(u.stem));
            list.insert(0, entry);
            list.truncate(VARIANTS_KEPT);
        }
        let log = self.v["rebuild_log"].as_object_mut().expect("index shape is validated on load");
        for r in rebuilds["units"].as_array().into_iter().flatten() {
            if r["reason"] == json!("new") {
                continue;
            }
            let Some(key) = r["logical_key"].as_str() else { continue };
            let list = log.entry(key.to_string()).or_insert_with(|| json!([]));
            let list = list.as_array_mut().expect("rebuild log entries are arrays");
            list.push(json!({ "run": run, "reason": r["reason"], "cost_ns": r["cost_ns"], "unit": r["package"], "kind": r["kind"] }));
            let excess = list.len().saturating_sub(REBUILD_LOG_KEPT);
            list.drain(..excess);
        }
    }
}

fn changed_files(now: &BTreeMap<String, (String, u64)>, before: &Value) -> (Vec<String>, Vec<String>) {
    let mut content = Vec::new();
    let mut mtime_only = Vec::new();
    let empty = Map::new();
    let before = before.as_object().unwrap_or(&empty);
    for (path, (digest, mtime)) in now {
        match before.get(path) {
            None => content.push(format!("{} (added)", path)),
            Some(old) if old[0].as_str() != Some(digest) => content.push(format!("{} (modified)", path)),
            Some(old) if old[1].as_u64() != Some(*mtime) => mtime_only.push(path.clone()),
            _ => {}
        }
    }
    for path in before.keys() {
        if !now.contains_key(path) {
            content.push(format!("{} (removed)", path));
        }
    }
    (content, mtime_only)
}

fn sample(list: &[String], n: usize) -> String {
    let mut s = list.iter().take(n).cloned().collect::<Vec<_>>().join(", ");
    if list.len() > n {
        s.push_str(&format!(", … (+{})", list.len() - n));
    }
    s
}

/// Classifies why each compiled unit ran, against its last known fingerprint (FR-040.1).
pub fn rebuilds(graph: &Graph, index: &Index, scripts: &[Value]) -> Value {
    let scripts_ran: HashSet<(&str, &str)> = scripts.iter().filter(|s| s["ran_in_session"] == json!(true)).filter_map(|s| Some((s["package"].as_str()?, s["version"].as_str()?))).collect();
    let rebuilt: HashSet<usize> = graph.units.iter().enumerate().filter(|(_, u)| u.built() || u.freshness == "failed").map(|(i, _)| i).collect();
    let mut out: Vec<Value> = Vec::new();
    let mut by_reason: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut known = 0u64;
    for (i, u) in graph.units.iter().enumerate() {
        if !rebuilt.contains(&i) || u.kind == "build-script-run" {
            continue;
        }
        let prior_list = index.entries(&u.logical_key);
        let cost = u.duration_ns.unwrap_or(0);
        let deps_rebuilt: Vec<String> = graph.deps[i].iter().filter(|d| rebuilt.contains(&d.idx) && graph.units[d.idx].kind != "build-script-run").map(|d| graph.units[d.idx].crate_name.clone()).collect();
        let script_ran = scripts_ran.contains(&(u.package.as_str(), u.version.as_str())) && u.kind != "build-script";
        let (reason, detail, differing, evidence, confidence): (&str, String, Vec<&str>, Vec<String>, &str) = if prior_list.is_empty() {
            ("new", "first time this unit is observed; nothing to compare against".into(), vec![], vec!["no earlier fingerprint in the unit index".into()], "high")
        } else if let Some(prior) = prior_list.iter().find(|p| p["exact_id"] == json!(u.exact_id)) {
            known += 1;
            let (_, mtime_only) = changed_files(&u.inputs, &prior["inputs"]);
            let prior_dir = prior["out_dir"].as_str().unwrap_or("");
            if prior_dir != u.out_dir {
                ("cache-miss", format!("an identical unit was built before in {}; its artifact is not shared with this build directory", prior_dir), vec![], vec!["every fingerprint component matches the earlier build".into(), "the output directory differs".into()], "high")
            } else if index.last_clean().map(|c| c > prior["wall_ns"].as_u64().unwrap_or(0)).unwrap_or(false) {
                ("cache-miss", "the artifact was removed by `cargo clean` after it was last built".into(), vec![], vec!["every fingerprint component matches the earlier build".into(), "a successful `cargo clean` was recorded since".into()], "high")
            } else if script_ran {
                ("build-script-input", "the package's build script ran again in this session".into(), vec![], vec!["build script execution observed (invoked.timestamp)".into(), "the unit's own sources are unchanged".into()], "medium")
            } else if !deps_rebuilt.is_empty() {
                ("dependency", format!("rebuilt because {} was rebuilt", sample(&deps_rebuilt, 4)), vec![], vec!["the unit's own fingerprint is unchanged".into(), "a direct dependency was recompiled in this session".into()], "high")
            } else if !mtime_only.is_empty() {
                ("source", format!("timestamp-only change (content identical): {}", sample(&mtime_only, 4)), vec!["source"], vec!["file modification times differ while content digests match".into()], "high")
            } else {
                (
                    "unknown",
                    "the fingerprint visible to QuickRust is unchanged".into(),
                    vec![],
                    vec!["evidence gap: Cargo's own fingerprint is not observable on a stable toolchain; possible causes include artifacts removed outside Cargo or inputs rustc did not list in its dep-info".into()],
                    "low",
                )
            }
        } else {
            known += 1;
            let f = &u.fingerprint;
            let components: [(&str, &str, &str); 9] = [
                ("compiler", "compiler", &f.compiler),
                ("target", "target", &f.target),
                ("profile", "profile", &f.profile),
                ("feature-set", "features", &f.features),
                ("rustflags", "rustflags", &f.rustflags),
                ("environment", "environment", &f.environment),
                ("source", "source", &f.source),
                ("generated-input", "generated", &f.generated),
                ("dependency", "dependencies", &f.dependencies),
            ];
            let diff = |p: &Value| -> Vec<&str> { components.iter().filter(|(_, key, now)| p["fingerprint"][*key].as_str() != Some(*now)).map(|(name, _, _)| *name).collect() };
            // Several variants of a unit may be known (other features, profiles, flags). The cause is
            // what separates this build from the nearest of them; ties go to the most recent.
            let prior = prior_list.iter().min_by_key(|p| diff(p).len()).expect("the list is not empty");
            let differing = diff(prior);
            let primary = differing.first().copied().unwrap_or("unknown");
            let (content, _) = changed_files(&u.inputs, &prior["inputs"]);
            let evidence = vec![format!("fingerprint components that differ from run {}: {}", util::vstr(prior, "run"), if differing.is_empty() { "none".to_string() } else { differing.join(", ") })];
            match primary {
                "source" => ("source", format!("changed: {}", sample(&content, 5)), differing, evidence, "high"),
                "feature-set" => {
                    let before: Vec<String> = prior["features"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect();
                    ("feature-set", format!("features [{}] → [{}]", before.join(", "), u.features.join(", ")), differing, evidence, "high")
                }
                "compiler" => ("compiler", "the compiler binary or selected toolchain changed".into(), differing, evidence, "high"),
                "profile" => {
                    let before = prior["codegen"].as_object().map(|o| o.iter().map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or(""))).collect::<Vec<_>>().join(" ")).unwrap_or_default();
                    let now = u.codegen.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join(" ");
                    ("profile", format!("profile settings [{}] → [{}]", before, now), differing, evidence, "high")
                }
                "generated-input" | "dependency" if script_ran => ("build-script-input", "the package's build script ran again and its outputs feed this unit".into(), differing, evidence, "medium"),
                "dependency" => ("dependency", "the set of dependency artifacts this unit links against changed".into(), differing, evidence, "high"),
                "unknown" => ("unknown", "the unit identity changed without a visible component difference".into(), differing, evidence, "low"),
                other => (other, format!("{} changed since run {}", other, util::vstr(prior, "run")), differing, evidence, "high"),
            }
        };
        let e = by_reason.entry(reason.to_string()).or_default();
        e.0 += 1;
        e.1 += cost;
        out.push(json!({
            "unit_id": u.id, "logical_key": u.logical_key, "package": u.package, "version": u.version, "kind": u.kind, "mode": u.mode,
            "reason": reason, "detail": detail, "differing": differing, "evidence": evidence, "confidence": confidence, "cost_ns": cost,
        }));
    }
    out.sort_by(|a, b| b["cost_ns"].as_u64().cmp(&a["cost_ns"].as_u64()));
    let index_units = index.all().count() as u64;
    let rebuilt_known_ns: u64 = by_reason.iter().filter(|(r, _)| r.as_str() != "new").map(|(_, v)| v.1).sum();
    json!({
        "summary": {
            "units_compiled": out.len(),
            "previously_known": known,
            "known_units_in_index": index_units,
            "invalidated_ratio": if index_units > 0 { json!(((known as f64 / index_units as f64) * 1000.0).round() / 1000.0) } else { Value::Null },
            "rebuild_time_of_known_units_ns": rebuilt_known_ns,
            "by_reason": by_reason.iter().map(|(r, (n, t))| (r.clone(), json!({ "units": n, "time_ns": t }))).collect::<Map<_, _>>(),
        },
        "units": out,
    })
}

fn script_key(s: &Value) -> String {
    format!("{}@{}", util::vstr(s, "package"), util::vstr(s, "version"))
}

/// Adds rerun history and the over-invalidation state to each build script (FR-060.1).
pub fn annotate_build_scripts(scripts: &mut [Value], index: &mut Index) {
    for s in scripts.iter_mut() {
        let key = script_key(s);
        let mut hist = index.build_script_history(&key).clone();
        if !hist.is_object() {
            hist = json!({ "runs": 0, "total_ns": 0, "runs_without_directives": 0 });
        }
        let ran = s["ran_in_session"] == json!(true);
        if ran {
            hist["runs"] = json!(hist["runs"].as_u64().unwrap_or(0) + 1);
            hist["total_ns"] = json!(hist["total_ns"].as_u64().unwrap_or(0) + s["duration_ns"].as_u64().unwrap_or(0));
            if s["has_rerun_directives"] != json!(true) {
                hist["runs_without_directives"] = json!(hist["runs_without_directives"].as_u64().unwrap_or(0) + 1);
            }
        }
        let reruns = hist["runs_without_directives"].as_u64().unwrap_or(0);
        s["state"] = json!(if !ran {
            "not-run"
        } else if s["has_rerun_directives"] != json!(true) && reruns >= 2 {
            // Without rerun-if-* directives Cargo reruns the script on any change in the package.
            "over-invalidating-candidate"
        } else {
            "observed"
        });
        s["history"] = hist.clone();
        index.v["build_scripts"][key] = hist;
    }
}

fn semver_compatible(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parts(a), parts(b));
    let get = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    if get(&a, 0) != get(&b, 0) {
        return false;
    }
    if get(&a, 0) > 0 {
        return true;
    }
    get(&a, 1) == get(&b, 1) && (get(&a, 1) > 0 || get(&a, 2) == get(&b, 2))
}

/// Shortest chain of dependents from a package up to a workspace member (FR-050.5).
fn reverse_path(pkg: &str, rev: &HashMap<&str, Vec<(&str, String)>>, members: &HashSet<&str>, label: &dyn Fn(&str) -> String) -> Vec<String> {
    let mut prev: HashMap<&str, (&str, &str)> = HashMap::new();
    let mut queue = VecDeque::from([pkg]);
    let mut seen: HashSet<&str> = HashSet::from([pkg]);
    let mut found = None;
    while let Some(cur) = queue.pop_front() {
        if members.contains(cur) && cur != pkg {
            found = Some(cur);
            break;
        }
        for (parent, kind) in rev.get(cur).into_iter().flatten() {
            if seen.insert(parent) {
                prev.insert(parent, (cur, kind.as_str()));
                queue.push_back(parent);
            }
        }
    }
    let Some(mut cur) = found else { return if members.contains(pkg) { vec![label(pkg)] } else { Vec::new() } };
    let mut path = vec![label(cur)];
    while let Some((child, kind)) = prev.get(cur) {
        path.push(if kind.is_empty() { label(child) } else { format!("{} [{}]", label(child), kind) });
        cur = child;
    }
    path
}

fn unit_view(u: &Unit) -> Value {
    json!({
        "stem": u.stem, "package": u.package, "version": u.version, "crate_name": u.crate_name, "kind": u.kind, "mode": u.mode, "role": u.role,
        "target": u.target, "features": u.features, "codegen": u.codegen, "dur_ns": u.duration_ns, "bytes": u.output_bytes, "out_dir": u.out_dir,
        "fingerprint": u.fingerprint, "observed_in": "this run",
    })
}

fn artifact_present(v: &Value) -> bool {
    let (dir, stem) = (Path::new(v["out_dir"].as_str().unwrap_or("")), v["stem"].as_str().unwrap_or(""));
    [format!("lib{}.rmeta", stem), format!("lib{}.rlib", stem), stem.to_string(), format!("lib{}.dylib", stem), format!("lib{}.so", stem), format!("{}.dll", stem), format!("{}.exe", stem)].iter().any(|f| dir.join(f).exists())
}

/// Multiple versions and multiple build variants of the same dependency (FR-050).
pub fn duplication(meta: Option<&Metadata>, graph: &Graph, index: &Index) -> Value {
    // Observed variants: this run's units plus earlier ones whose artifacts still exist.
    let mut variants: BTreeMap<String, Value> = BTreeMap::new();
    for e in index.all() {
        if artifact_present(e) {
            let mut v = e.clone();
            v["observed_in"] = json!(format!("run {}", util::vstr(e, "run")));
            if let Some(o) = v.as_object_mut() {
                o.remove("inputs");
            }
            variants.insert(util::vstr(e, "stem"), v);
        }
    }
    for u in graph.units.iter().filter(|u| u.built() && u.kind != "build-script-run") {
        variants.insert(u.stem.clone(), unit_view(u));
    }
    let mut consumers: HashMap<&str, Vec<String>> = HashMap::new();
    for (i, ds) in graph.deps.iter().enumerate() {
        for d in ds {
            consumers.entry(graph.units[d.idx].stem.as_str()).or_default().push(graph.units[i].crate_name.clone());
        }
    }
    let mut groups: BTreeMap<(String, String, String, String), Vec<&Value>> = BTreeMap::new();
    for v in variants.values() {
        groups.entry((util::vstr(v, "package"), util::vstr(v, "version"), util::vstr(v, "crate_name"), util::vstr(v, "kind"))).or_default().push(v);
    }
    let mut variant_groups = Vec::new();
    let (mut dup_units, mut dup_ns, mut dup_bytes) = (0u64, 0u64, 0u64);
    for ((package, version, crate_name, kind), list) in &groups {
        if list.len() < 2 || kind == "build-script" {
            continue;
        }
        let first = list[0];
        let mut causes: BTreeSet<&str> = BTreeSet::new();
        for other in &list[1..] {
            let diff = |k: &str| first["fingerprint"][k] != other["fingerprint"][k];
            if first["mode"] != other["mode"] {
                causes.insert("activity");
            }
            if diff("target") || first["role"] != other["role"] {
                causes.insert("host-or-target");
            }
            if diff("features") {
                causes.insert("feature-set");
            }
            if diff("profile") && first["mode"] == other["mode"] {
                causes.insert("profile");
            }
            if diff("rustflags") {
                causes.insert("compiler-flags");
            }
            if diff("compiler") {
                causes.insert("compiler");
            }
        }
        if causes.is_empty() {
            causes.insert("unknown");
        }
        // Reducible only when nothing but the feature set or ad-hoc flags separates the variants.
        let state = if causes.iter().all(|c| matches!(*c, "feature-set" | "compiler-flags" | "profile")) {
            "potentially-reducible"
        } else if causes.contains("unknown") {
            "unknown"
        } else {
            "necessary"
        };
        let durs: Vec<u64> = list.iter().map(|v| v["dur_ns"].as_u64().unwrap_or(0)).collect();
        let bytes: Vec<u64> = list.iter().map(|v| v["bytes"].as_u64().unwrap_or(0)).collect();
        dup_units += list.len() as u64 - 1;
        dup_ns += durs.iter().sum::<u64>() - durs.iter().max().copied().unwrap_or(0);
        dup_bytes += bytes.iter().sum::<u64>() - bytes.iter().max().copied().unwrap_or(0);
        variant_groups.push(json!({
            "package": package, "version": version, "crate_name": crate_name, "kind": kind,
            "causes": causes, "state": state,
            "explanation": match state {
                "necessary" => "variants differ by host/target role, compiler or build activity (check vs build); rustc cannot share these today",
                "potentially-reducible" => "variants differ only in features, profile or flags; aligning what the dependents request would build this once",
                _ => "the difference between these variants is not visible in the recorded evidence",
            },
            "total_time_ns": durs.iter().sum::<u64>(),
            "total_bytes": bytes.iter().sum::<u64>(),
            "variants": list.iter().map(|v| json!({
                "stem": v["stem"], "mode": v["mode"], "role": v["role"], "target": v["target"], "features": v["features"], "codegen": v["codegen"],
                "dur_ns": v["dur_ns"], "bytes": v["bytes"], "observed_in": v["observed_in"],
                // Which crates ask for this variant: required before any feature advice (FR-050.6).
                "dependents": consumers.get(v["stem"].as_str().unwrap_or("")).cloned().unwrap_or_default(),
            })).collect::<Vec<_>>(),
        }));
    }
    variant_groups.sort_by(|a, b| b["total_time_ns"].as_u64().cmp(&a["total_time_ns"].as_u64()));

    // Multiple versions, from Cargo's own resolution.
    let mut versions = Value::Null;
    if let Some(m) = meta.filter(|m| m.with_deps) {
        let pkgs = m.value["packages"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let info: HashMap<&str, &Value> = pkgs.iter().filter_map(|p| Some((p["id"].as_str()?, p))).collect();
        let label = |id: &str| info.get(id).map(|p| format!("{}@{}", util::vstr(p, "name"), util::vstr(p, "version"))).unwrap_or_else(|| id.to_string());
        let members: HashSet<&str> = m.value["workspace_members"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let mut rev: HashMap<&str, Vec<(&str, String)>> = HashMap::new();
        for node in m.value["resolve"]["nodes"].as_array().into_iter().flatten() {
            let Some(parent) = node["id"].as_str() else { continue };
            for dep in node["deps"].as_array().into_iter().flatten() {
                let Some(child) = dep["pkg"].as_str() else { continue };
                let kinds: BTreeSet<&str> = dep["dep_kinds"].as_array().into_iter().flatten().map(|k| k["kind"].as_str().unwrap_or("normal")).collect();
                let kind = if kinds.contains("normal") { String::new() } else { kinds.into_iter().collect::<Vec<_>>().join("+") };
                rev.entry(child).or_default().push((parent, kind));
            }
        }
        let mut by_name: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
        for p in pkgs {
            if let Some(n) = p["name"].as_str() {
                by_name.entry(n).or_default().push(p);
            }
        }
        let cost = |name: &str, version: &str| -> (Option<u64>, Option<u64>) {
            let matching: Vec<&Value> = variants.values().filter(|v| v["package"] == json!(name) && v["version"] == json!(version)).collect();
            if matching.is_empty() {
                (None, None)
            } else {
                (Some(matching.iter().filter_map(|v| v["dur_ns"].as_u64()).sum()), Some(matching.iter().filter_map(|v| v["bytes"].as_u64()).sum()))
            }
        };
        let mut list = Vec::new();
        for (name, ps) in by_name {
            if ps.len() < 2 {
                continue;
            }
            let vs: Vec<String> = ps.iter().map(|p| util::vstr(p, "version")).collect();
            // Cargo resolves one SemVer-compatible version per source: compatible versions of one
            // crate side by side always come from different sources (registry, git, path), which
            // the lockfile cannot unify.
            let compatible = vs.iter().all(|v| semver_compatible(v, &vs[0]));
            list.push(json!({
                "name": name,
                "state": if compatible { "separate-sources" } else { "necessary" },
                "explanation": if compatible {
                    "the versions are SemVer-compatible but come from different sources (registry, git or path), which Cargo never unifies; pointing every dependent at one source, or a `[patch]` entry, would build it once"
                } else {
                    "the versions are SemVer-incompatible; each stays until the dependents on one path move to the other's major version"
                },
                "versions": ps.iter().map(|p| {
                    let id = p["id"].as_str().unwrap_or("");
                    let (ns, bytes) = cost(name, p["version"].as_str().unwrap_or(""));
                    json!({
                        "version": p["version"], "source": p["source"],
                        "required_by": reverse_path(id, &rev, &members, &label),
                        "direct_dependents": rev.get(id).map(|r| r.iter().map(|(parent, _)| label(parent)).collect::<BTreeSet<_>>()).unwrap_or_default(),
                        "observed_compile_ns": ns, "observed_bytes": bytes,
                    })
                }).collect::<Vec<_>>(),
            }));
        }
        versions = json!(list);
    }
    json!({
        "versions": versions,
        "variants": variant_groups,
        "summary": {
            "packages_with_multiple_versions": versions.as_array().map(Vec::len),
            "variant_groups": variant_groups.len(),
            "duplicate_units": dup_units,
            "duplicate_time_ns": dup_ns,
            "duplicate_bytes": dup_bytes,
            "basis": "variants observed by QuickRust whose artifacts still exist; each group keeps its most expensive variant as the necessary one",
        },
    })
}

fn transitive_dependents(graph: &Graph, root: usize) -> HashSet<usize> {
    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); graph.units.len()];
    for (i, ds) in graph.deps.iter().enumerate() {
        for d in ds {
            consumers[d.idx].push(i);
        }
    }
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(u) = stack.pop() {
        for &c in &consumers[u] {
            if seen.insert(c) {
                stack.push(c);
            }
        }
    }
    seen
}

/// Proc-macro crates: their own compile cost, kept apart from what waits on them (FR-070.1).
pub fn proc_macros(graph: &Graph, cp: &CriticalPath) -> Value {
    let on_path: HashSet<usize> = cp.path.iter().map(|(i, _)| *i).collect();
    let mut list = Vec::new();
    for (i, u) in graph.units.iter().enumerate().filter(|(_, u)| u.kind == "proc-macro") {
        let downstream = transitive_dependents(graph, i);
        let direct = graph.deps.iter().filter(|ds| ds.iter().any(|d| d.idx == i)).count();
        list.push(json!({
            "unit_id": u.id, "package": u.package, "version": u.version, "freshness": u.freshness,
            "compile_ns": u.duration_ns,
            "on_critical_path": on_path.contains(&i),
            "state": if on_path.contains(&i) { "critical-path-candidate" } else if u.duration_ns.unwrap_or(0) >= 5_000_000_000 { "expensive" } else { "observed" },
            "direct_dependents": direct,
            "downstream_units": downstream.len(),
            "downstream_compile_ns": downstream.iter().filter_map(|&d| graph.units[d].duration_ns).sum::<u64>(),
            "cacheable_by_sccache": false,
            "cache_note": "proc-macro crates invoke the system linker and are not cached by sccache; macros that read files are a caching hazard in the crates that use them",
            "execution_time_ns": null,
        }));
    }
    list.sort_by(|a, b| b["compile_ns"].as_u64().cmp(&a["compile_ns"].as_u64()));
    json!({
        "units": list,
        "unavailable": { "execution_time_ns": "a stable toolchain does not expose time spent expanding macros in dependent crates; only the macro crate's own compilation is measured" },
    })
}

/// Effective codegen settings and linking units, as actually passed to rustc (FR-080).
pub fn codegen(graph: &Graph, session: &Value) -> Value {
    let profile_name = if session["command"]["release"] == json!(true) { "release" } else { "dev" };
    let mut groups: BTreeMap<String, (Value, u64, u64, u64, u64)> = BTreeMap::new();
    let mut link_units = Vec::new();
    let mut link_ns = 0u64;
    for u in graph.units.iter().filter(|u| u.built() && u.kind != "build-script-run") {
        let get = |k: &str, default: &str| u.codegen.get(k).cloned().unwrap_or_else(|| default.to_string());
        let sig = json!({
            "opt_level": get("opt-level", "0"), "debuginfo": get("debuginfo", "0"), "codegen_units": u.codegen.get("codegen-units"),
            "lto": u.codegen.get("lto"), "panic": get("panic", "unwind"), "incremental": u.incremental,
            "split_debuginfo": u.codegen.get("split-debuginfo"), "strip": u.codegen.get("strip"),
            "class": if matches!(get("opt-level", "0").as_str(), "0" | "1") { "development" } else { "optimized" },
            "role": u.role,
        });
        let g = groups.entry(sig.to_string()).or_insert_with(|| (sig, 0, 0, 0, 0));
        g.1 += 1;
        g.2 += u.duration_ns.unwrap_or(0);
        g.3 += u.output_bytes.unwrap_or(0);
        g.4 += u64::from(u.source == "path");
        if u.links {
            link_ns += u.duration_ns.unwrap_or(0);
            link_units.push(json!({
                "unit_id": u.id, "package": u.package, "kind": u.kind, "duration_ns": u.duration_ns, "output_bytes": u.output_bytes,
                "linker": u.linker.clone().unwrap_or_else(|| "platform default".into()), "lto": u.codegen.get("lto"), "on_critical_path": u.on_critical_path,
            }));
        }
    }
    link_units.sort_by(|a, b| b["duration_ns"].as_u64().cmp(&a["duration_ns"].as_u64()));
    let mut settings: Vec<Value> = groups
        .into_values()
        .map(|(sig, n, ns, bytes, local)| json!({ "settings": sig, "units": n, "workspace_units": local, "dependency_units": n - local, "time_ns": ns, "output_bytes": bytes }))
        .collect();
    settings.sort_by(|a, b| b["time_ns"].as_u64().cmp(&a["time_ns"].as_u64()));
    json!({
        "requested_profile": profile_name,
        "settings": settings,
        "linking": {
            "units": link_units,
            "linking_unit_time_ns": link_ns,
            "unavailable": { "link_phase_ns": "rustc does not report the linker's share of a unit on a stable toolchain; the whole unit duration is shown" },
        },
    })
}

/// The effective incremental decision and whether it conflicts with the cache strategy (FR-090, AC-010).
pub fn incremental_policy(graph: &Graph, session: &Value) -> Value {
    let compiled: Vec<&Unit> = graph.units.iter().filter(|u| u.built() && u.kind != "build-script-run").collect();
    let inc: Vec<&&Unit> = compiled.iter().filter(|u| u.incremental).collect();
    let sccache = session["policy"]["cache"]["sccache_active"] == json!(true);
    let conflict = sccache && !inc.is_empty();
    let mut v = session["policy"]["incremental"].clone();
    if !v.is_object() {
        v = json!({});
    }
    v["observed"] = json!({
        "units_compiled": compiled.len(),
        "units_incremental": inc.len(),
        "incremental_time_ns": inc.iter().filter_map(|u| u.duration_ns).sum::<u64>(),
    });
    v["sccache_conflict"] = json!(conflict);
    v["explanation"] = json!(if conflict {
        format!(
            "{} unit(s) were compiled incrementally; sccache cannot cache incremental rustc invocations, so those units always compile locally. \
             Incremental state speeds up repeated edits in one checkout, sccache reuses whole results across checkouts and machines: pick per workload and measure with `qrust experiment`.",
            inc.len()
        )
    } else if sccache {
        "sccache is active and no unit was compiled incrementally; every non-linking unit is eligible for caching".to_string()
    } else if inc.is_empty() {
        "no unit was compiled incrementally".to_string()
    } else {
        format!("{} unit(s) compiled incrementally (Cargo's default for workspace crates in the dev profile); no compiler cache is active, so there is no conflict", inc.len())
    });
    v
}

/// Same-repository worktrees and other known workspaces: duplicated units and shared state (FR-120).
pub fn worktrees(store: &WsStore, cfg: &crate::config::Config) -> Result<Value, String> {
    let root = store.root.as_ref().ok_or("not inside a Cargo workspace")?;
    let root = util::canon(root);
    let git = |dir: &Path, args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).stderr(std::process::Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let repo_of = |dir: &Path| git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let this_repo = repo_of(&root);
    // Candidate roots: linked worktrees of this repository plus every workspace QuickRust has recorded.
    let mut candidates: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    if let (Some(top), Some(list)) = (git(&root, &["rev-parse", "--show-toplevel"]), git(&root, &["worktree", "list", "--porcelain"])) {
        let rel = root.strip_prefix(util::canon(Path::new(&top))).unwrap_or(Path::new("")).to_path_buf();
        for line in list.lines() {
            if let Some(wt) = line.strip_prefix("worktree ") {
                let candidate = util::canon(&Path::new(wt).join(&rel));
                if candidate.join("Cargo.toml").is_file() {
                    candidates.insert(candidate);
                }
            }
        }
    }
    for (_, known_root) in store::known_workspaces() {
        candidates.insert(util::canon(&known_root));
    }
    candidates.remove(&root);

    let describe = |ws: &WsStore| -> (Option<String>, Vec<(u64, u64)>) {
        let ids = ws.session_ids();
        let target = ids.iter().rev().take(20).find_map(|id| ws.load_run(id).and_then(|r| r["workspace"]["build_directory"].as_str().map(str::to_string)));
        let spans = ids.iter().rev().take(50).filter_map(|id| ws.load_session(id)).filter_map(|s| Some((s["started_wall_ns"].as_u64()?, s["started_wall_ns"].as_u64()? + s["duration_ns"].as_u64()?))).collect();
        (target, spans)
    };
    let own_index = Index::load(&store.index_path())?;
    let own: HashMap<String, &Value> = own_index.all().filter_map(|e| Some((e["portable_id"].as_str()?.to_string(), e))).collect();
    let (own_target, own_spans) = describe(store);
    let mut others = Vec::new();
    let (mut total_units, mut total_ns, mut total_bytes) = (0u64, 0u64, 0u64);
    for other_root in candidates {
        let other = WsStore::open(Some(&other_root));
        // Bring the other workspace's unit index up to date with what it has built.
        if let Some(latest) = other.session_ids().last() {
            if let Err(e) = crate::analyze::finalize(&other, latest, cfg) {
                others.push(json!({ "root": path_str(&other_root), "state": "unknown", "error": e }));
                continue;
            }
        }
        let same_repo = match (&this_repo, repo_of(&other_root)) {
            // Repository identity is the shared git directory, never the remote URL (FR-120 validation).
            (Some(a), Some(b)) => Some(*a == b),
            _ => None,
        };
        let index = match Index::load(&other.index_path()) {
            Ok(i) => i,
            Err(e) => {
                others.push(json!({ "root": path_str(&other_root), "state": "unknown", "error": e }));
                continue;
            }
        };
        let (target, spans) = describe(&other);
        let shared: Vec<&Value> = index.all().filter(|e| e["portable_id"].as_str().map(|p| own.contains_key(p)).unwrap_or(false)).collect();
        let time: u64 = shared.iter().filter_map(|e| e["dur_ns"].as_u64()).sum();
        let bytes: u64 = shared.iter().filter_map(|e| e["bytes"].as_u64()).sum();
        let state = match (&own_target, &target) {
            (Some(a), Some(b)) if a == b => {
                if own_spans.iter().any(|(s1, e1)| spans.iter().any(|(s2, e2)| s1 < e2 && s2 < e1)) {
                    "shared-contended"
                } else {
                    "shared-safe"
                }
            }
            (Some(_), Some(_)) => "isolated",
            _ => "unknown",
        };
        // Units built separately in both places are duplicates only when the build directories differ.
        let duplicated = state == "isolated";
        if duplicated {
            total_units += shared.len() as u64;
            total_ns += time;
            total_bytes += bytes;
        }
        let mut sample: Vec<String> = shared.iter().map(|e| format!("{}@{}", util::vstr(e, "package"), util::vstr(e, "version"))).collect::<BTreeSet<_>>().into_iter().collect();
        sample.truncate(15);
        others.push(json!({
            "root": path_str(&other_root),
            "same_repository": same_repo,
            "build_directory": target,
            "state": state,
            "recorded_units": index.all().count(),
            "identical_units": shared.len(),
            "identical_unit_compile_ns": time,
            "identical_unit_bytes": bytes,
            "counts_as_duplicate": duplicated,
            "sample": sample,
        }));
    }
    let advice = if total_units > 0 {
        json!([
            "identical units are compiled and stored once per build directory; a compiler cache (cache.provider = \"sccache\") reuses those results across directories without sharing writable state",
            "QuickRust does not point several worktrees at one writable target directory: Cargo serialises builds there with a lock, and different branches would keep invalidating each other",
            "Cargo's planned cross-workspace cache addresses this natively; QuickRust will delegate to it when it is available on the selected toolchain",
        ])
    } else {
        json!([])
    };
    Ok(json!({
        "workspace": path_str(&root),
        "build_directory": own_target,
        "repository_detected": this_repo.is_some(),
        "others": others,
        "summary": { "duplicate_units": total_units, "duplicate_compile_ns": total_ns, "duplicate_bytes": total_bytes },
        "advice": advice,
        "unavailable": if this_repo.is_none() { json!({ "worktrees": "not a git repository; only workspaces previously built through QuickRust are compared" }) } else { json!({}) },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::{build_graph, parse_unit};
    use crate::util::Tokenizer;

    struct Ev {
        name: &'static str,
        hash: &'static str,
        externs: Vec<(&'static str, &'static str)>,
        extra: Vec<&'static str>,
        files: Vec<(&'static str, &'static str, u64)>,
        out: &'static str,
        rustc_mtime: u64,
    }

    impl Ev {
        fn new(name: &'static str, hash: &'static str) -> Ev {
            Ev { name, hash, externs: vec![], extra: vec![], files: vec![("src/lib.rs", "aaaa", 1)], out: "/t/debug/deps", rustc_mtime: 2 }
        }
        fn json(&self) -> Value {
            let mut args: Vec<String> = vec!["--crate-name".into(), self.name.into(), "--crate-type".into(), "lib".into(), "--emit=dep-info,metadata,link".into(), "-C".into(), format!("metadata={}", self.hash), "-C".into(), format!("extra-filename=-{}", self.hash), "--out-dir".into(), self.out.into()];
            for (n, f) in &self.externs {
                args.push("--extern".into());
                args.push(format!("{}={}/{}", n, self.out, f));
            }
            args.extend(self.extra.iter().map(|s| s.to_string()));
            json!({ "kind": "compile", "args": args, "cwd": "/w", "exit_code": 0, "start_wall_ns": 10, "dur_ns": 100,
                "rustc": "/tc/bin/rustc", "rustc_id": { "bytes": 1, "mtime_ns": self.rustc_mtime },
                "env": { "pkg_name": self.name, "pkg_version": "1.0.0", "manifest_dir": format!("/w/{}", self.name), "toolchain": "stable" },
                "inputs": { "local": self.files.iter().map(|(p, sha, m)| json!({ "path": p, "sha": sha, "mtime_ns": m, "bytes": 1 })).collect::<Vec<_>>(), "generated": [], "env_deps": [] } })
        }
    }

    fn graph(evs: &[Ev]) -> Graph {
        let tok = Tokenizer::new(None, false);
        build_graph(evs.iter().map(|e| parse_unit(&e.json(), false, &tok).unwrap()).collect(), 0, 1 << 60, &HashMap::new()).0
    }

    /// Runs `first` then `second` through the index and returns the second run's classification by crate.
    fn reasons(first: &[Ev], second: &[Ev], clean_between: bool) -> HashMap<String, (String, String)> {
        let mut index = Index::empty();
        let g1 = graph(first);
        let r1 = rebuilds(&g1, &index, &[]);
        index.absorb(&g1, &r1, "run1", 100, "build", true);
        if clean_between {
            index.absorb(&graph(&[]), &json!({}), "clean", 200, "clean", true);
        }
        let r2 = rebuilds(&graph(second), &index, &[]);
        r2["units"].as_array().unwrap().iter().map(|u| (util::vstr(u, "package"), (util::vstr(u, "reason"), util::vstr(u, "detail")))).collect()
    }

    #[test]
    fn first_observation_is_new_not_a_rebuild() {
        let r = rebuilds(&graph(&[Ev::new("a", "1")]), &Index::empty(), &[]);
        assert_eq!(r["units"][0]["reason"], "new");
        assert_eq!(r["summary"]["previously_known"], 0);
    }

    #[test]
    fn source_edit_names_the_file_and_dependents_blame_the_dependency() {
        let mut a2 = Ev::new("a", "1");
        a2.files = vec![("src/lib.rs", "bbbb", 9)];
        let mut b = Ev::new("b", "2");
        b.externs = vec![("a", "liba-1.rlib")];
        let mut b2 = Ev::new("b", "2");
        b2.externs = vec![("a", "liba-1.rlib")];
        let r = reasons(&[Ev::new("a", "1"), b], &[a2, b2], false);
        assert_eq!(r["a"].0, "source");
        assert!(r["a"].1.contains("src/lib.rs (modified)"), "{}", r["a"].1);
        assert_eq!(r["b"].0, "dependency");
        assert!(r["b"].1.contains('a'));
    }

    #[test]
    fn toolchain_change_is_reported_as_compiler() {
        // FR-040 acceptance: a toolchain change is reported as toolchain drift.
        let mut a2 = Ev::new("a", "1");
        a2.rustc_mtime = 77;
        assert_eq!(reasons(&[Ev::new("a", "1")], &[a2], false)["a"].0, "compiler");
    }

    #[test]
    fn features_profile_and_flags_are_told_apart() {
        let mut feat = Ev::new("a", "1");
        feat.extra = vec!["--cfg", "feature=\"std\""];
        let r = reasons(&[Ev::new("a", "1")], &[feat], false);
        assert_eq!(r["a"].0, "feature-set");
        assert!(r["a"].1.contains("[] → [std]"), "{}", r["a"].1);
        let mut prof = Ev::new("a", "1");
        prof.extra = vec!["-C", "opt-level=3"];
        assert_eq!(reasons(&[Ev::new("a", "1")], &[prof], false)["a"].0, "profile");
        let mut flags = Ev::new("a", "1");
        flags.extra = vec!["--cfg", "tokio_unstable"];
        assert_eq!(reasons(&[Ev::new("a", "1")], &[flags], false)["a"].0, "rustflags");
    }

    #[test]
    fn identical_unit_after_clean_or_elsewhere_is_a_cache_miss() {
        assert_eq!(reasons(&[Ev::new("a", "1")], &[Ev::new("a", "1")], true)["a"].0, "cache-miss");
        let mut elsewhere = Ev::new("a", "1");
        elsewhere.out = "/other/debug/deps";
        let r = reasons(&[Ev::new("a", "1")], &[elsewhere], false);
        assert_eq!(r["a"].0, "cache-miss");
        assert!(r["a"].1.contains("/t/debug/deps"));
    }

    #[test]
    fn timestamp_only_change_is_distinguished_and_no_evidence_means_unknown() {
        let mut touched = Ev::new("a", "1");
        touched.files = vec![("src/lib.rs", "aaaa", 999)];
        let r = reasons(&[Ev::new("a", "1")], &[touched], false);
        assert_eq!(r["a"].0, "source");
        assert!(r["a"].1.contains("timestamp-only"));
        // FR-040 acceptance: nothing trustworthy to say ⇒ "unknown", with the gap named.
        let mut index = Index::empty();
        let g = graph(&[Ev::new("a", "1")]);
        index.absorb(&g, &json!({}), "run1", 100, "build", true);
        let r = rebuilds(&graph(&[Ev::new("a", "1")]), &index, &[]);
        assert_eq!(r["units"][0]["reason"], "unknown");
        assert!(r["units"][0]["evidence"][0].as_str().unwrap().contains("evidence gap"));
    }

    #[test]
    fn feature_variants_are_not_version_duplicates() {
        // FR-050 acceptance: same package/version, different features ⇒ feature variants.
        let mut with = Ev::new("a", "2");
        with.extra = vec!["--cfg", "feature=\"std\""];
        let mut user = Ev::new("user", "3");
        user.externs = vec![("a", "liba-2.rlib")];
        let d = duplication(None, &graph(&[Ev::new("a", "1"), with, user]), &Index::empty());
        assert!(d["versions"].is_null(), "no resolution available ⇒ versions unavailable, not empty");
        let g = &d["variants"][0];
        assert_eq!(g["package"], "a");
        assert_eq!(g["causes"], json!(["feature-set"]));
        assert_eq!(g["state"], "potentially-reducible");
        let std_variant = g["variants"].as_array().unwrap().iter().find(|v| v["features"] == json!(["std"])).unwrap();
        assert_eq!(std_variant["dependents"], json!(["user"]), "the dependent enabling the feature must be identified");
        assert_eq!(d["summary"]["duplicate_units"], 1);
    }

    #[test]
    fn version_duplicates_show_both_reverse_paths() {
        // FR-050 acceptance: two incompatible versions required by distinct dependencies.
        let meta = Metadata {
            with_deps: true,
            deps_unavailable: None,
            value: json!({
                "workspace_members": ["app 0.1.0"],
                "packages": [
                    { "id": "app 0.1.0", "name": "app", "version": "0.1.0" }, { "id": "x 1.0.0", "name": "x", "version": "1.0.0" }, { "id": "y 1.0.0", "name": "y", "version": "1.0.0" },
                    { "id": "dup 1.2.0", "name": "dup", "version": "1.2.0", "source": "registry" }, { "id": "dup 2.0.0", "name": "dup", "version": "2.0.0", "source": "registry" }
                ],
                "resolve": { "nodes": [
                    { "id": "app 0.1.0", "deps": [{ "pkg": "x 1.0.0", "dep_kinds": [{ "kind": null }] }, { "pkg": "y 1.0.0", "dep_kinds": [{ "kind": "build" }] }] },
                    { "id": "x 1.0.0", "deps": [{ "pkg": "dup 1.2.0", "dep_kinds": [{ "kind": null }] }] },
                    { "id": "y 1.0.0", "deps": [{ "pkg": "dup 2.0.0", "dep_kinds": [{ "kind": null }] }] },
                    { "id": "dup 1.2.0", "deps": [] }, { "id": "dup 2.0.0", "deps": [] }
                ] }
            }),
        };
        let d = duplication(Some(&meta), &graph(&[]), &Index::empty());
        let dup = &d["versions"][0];
        assert_eq!(dup["name"], "dup");
        assert_eq!(dup["state"], "necessary");
        assert_eq!(dup["versions"][0]["required_by"], json!(["app@0.1.0", "x@1.0.0", "dup@1.2.0"]));
        assert_eq!(dup["versions"][1]["required_by"], json!(["app@0.1.0", "y@1.0.0 [build]", "dup@2.0.0"]));
        assert!(dup["versions"][0]["observed_compile_ns"].is_null(), "cost is unavailable when the unit was never observed");
    }

    #[test]
    fn semver_compatibility() {
        assert!(semver_compatible("1.2.0", "1.9.3"));
        assert!(!semver_compatible("1.2.0", "2.0.0"));
        assert!(semver_compatible("0.4.1", "0.4.9"));
        assert!(!semver_compatible("0.4.1", "0.5.0"));
        assert!(!semver_compatible("0.0.1", "0.0.2"));
    }

    #[test]
    fn build_script_without_directives_becomes_a_candidate_only_when_it_reruns() {
        let script = || json!({ "package": "p", "version": "1.0.0", "ran_in_session": true, "has_rerun_directives": false, "duration_ns": 5 });
        let mut index = Index::empty();
        let mut first = vec![script()];
        annotate_build_scripts(&mut first, &mut index);
        assert_eq!(first[0]["state"], "observed", "a single run is not evidence of repeated cost");
        let mut second = vec![script()];
        annotate_build_scripts(&mut second, &mut index);
        assert_eq!(second[0]["state"], "over-invalidating-candidate");
        assert_eq!(second[0]["history"]["total_ns"], 10);
        let mut precise = vec![json!({ "package": "q", "version": "1.0.0", "ran_in_session": true, "has_rerun_directives": true, "duration_ns": 5 })];
        annotate_build_scripts(&mut precise, &mut index);
        annotate_build_scripts(&mut precise, &mut index);
        assert_eq!(precise[0]["state"], "observed");
    }

    #[test]
    fn proc_macro_cost_is_separate_from_what_waits_on_it() {
        let mut mac = Ev::new("mac", "1");
        mac.extra = vec!["--crate-type", "proc-macro"];
        let mut a = Ev::new("a", "2");
        a.externs = vec![("mac", "libmac-1.dylib")];
        let mut b = Ev::new("b", "3");
        b.externs = vec![("a", "liba-2.rlib")];
        let g = graph(&[mac, a, b]);
        let cp = crate::analyze::critical_path(&g);
        let p = proc_macros(&g, &cp);
        let m = &p["units"][0];
        assert_eq!(m["compile_ns"], 100);
        assert_eq!(m["direct_dependents"], 1);
        assert_eq!(m["downstream_units"], 2);
        assert_eq!(m["downstream_compile_ns"], 200);
        assert_eq!(m["on_critical_path"], true);
        assert_eq!(m["cacheable_by_sccache"], false);
        assert!(m["execution_time_ns"].is_null());
    }
}
