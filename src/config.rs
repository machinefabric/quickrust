//! Haul's own configuration namespace. Cargo's `.cargo/config.toml` is never
//! interpreted here for execution semantics (COMP-050).
//!
//! Precedence: built-in defaults < `$HAUL_HOME/config.toml`
//! < repository `.haul/config.toml` < `HAUL_*` environment < flags.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Str(String),
    Bool(bool),
    Int(i64),
}

impl std::fmt::Display for Val {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Val::Str(s) => write!(f, "{:?}", s),
            Val::Bool(b) => write!(f, "{}", b),
            Val::Int(i) => write!(f, "{}", i),
        }
    }
}

enum Kind {
    Str,
    Enum(&'static [&'static str]),
    Bool,
    Int,
}

struct Key {
    name: &'static str,
    kind: Kind,
    default: &'static str,
    /// Environment variable name fixed by the specification; otherwise it is derived from the key.
    env: Option<&'static str>,
    help: &'static str,
}

const KEYS: &[Key] = &[
    Key { name: "compatibility.nested-cargo", kind: Kind::Enum(&["orchestrator", "upstream"]), default: "orchestrator", env: Some("HAUL_NESTED"), help: "nested `cargo` invocations that reach the shim: record them (orchestrator) or hand straight to upstream" },
    Key { name: "compatibility.unknown-command", kind: Kind::Enum(&["delegate", "instrument"]), default: "delegate", env: None, help: "external subcommands: plain delegation or rustc-wrapper instrumentation" },
    Key { name: "compatibility.real-cargo", kind: Kind::Str, default: "", env: None, help: "absolute path of the upstream Cargo binary (resolution rule 4)" },
    Key { name: "measurement.enabled", kind: Kind::Bool, default: "true", env: None, help: "observe rustc invocations through the Haul rustc wrapper" },
    Key { name: "measurement.report", kind: Kind::Str, default: "", env: Some("HAUL_REPORT"), help: "sidecar report path; `{session}` expands to the run id" },
    Key { name: "measurement.finalize", kind: Kind::Enum(&["lazy", "sync"]), default: "lazy", env: None, help: "analyse runs on demand (lazy) or right after each build (sync)" },
    Key { name: "measurement.cargo-json", kind: Kind::Enum(&["auto", "off"]), default: "auto", env: None, help: "observe Cargo JSON messages when the user already requested them on a non-terminal stdout" },
    Key { name: "measurement.strict", kind: Kind::Bool, default: "false", env: None, help: "treat report/telemetry failures as command failures" },
    Key { name: "measurement.keep-runs", kind: Kind::Int, default: "200", env: None, help: "summarised runs retained per workspace" },
    Key { name: "measurement.keep-raw-events", kind: Kind::Int, default: "20", env: None, help: "runs whose raw rustc events are retained" },
    Key { name: "incremental.local-dev", kind: Kind::Enum(&["cargo-default", "on", "off", "experiment"]), default: "cargo-default", env: None, help: "incremental compilation policy on developer machines" },
    Key { name: "incremental.ci", kind: Kind::Enum(&["cargo-default", "on", "off", "experiment"]), default: "cargo-default", env: None, help: "incremental compilation policy in CI" },
    Key { name: "cache.provider", kind: Kind::Enum(&["inherit", "none", "sccache"]), default: "inherit", env: None, help: "compiler cache: observe what is configured, none, or chain sccache" },
    Key { name: "cache.fail", kind: Kind::Enum(&["open", "closed"]), default: "open", env: None, help: "on cache unavailability: compile locally (open) or fail (closed)" },
    Key { name: "artifacts.inventory", kind: Kind::Bool, default: "true", env: None, help: "include a build-directory inventory when a run is analysed" },
    Key { name: "artifacts.stale-days", kind: Kind::Int, default: "30", env: None, help: "age after which unused rebuildable artifacts become GC candidates" },
    Key { name: "ci.budget-policy", kind: Kind::Str, default: ".haul/budgets.toml", env: None, help: "budget file, relative to the workspace root" },
    Key { name: "ci.baseline", kind: Kind::Str, default: "main", env: None, help: "default baseline name for budget evaluation" },
    Key { name: "ci.enforce-on-build", kind: Kind::Bool, default: "false", env: None, help: "evaluate budgets after ordinary build commands and fail on violations" },
    Key { name: "security.persist-environment", kind: Kind::Enum(&["allowlist", "none"]), default: "allowlist", env: None, help: "which environment values may be persisted in run records" },
    Key { name: "security.redact-path-home", kind: Kind::Bool, default: "true", env: None, help: "tokenise home/workspace paths in exported reports" },
    Key { name: "benchmark.allow-mutation", kind: Kind::Bool, default: "false", env: None, help: "allow synthetic source edits without interactive confirmation" },
];

/// Variables in the `HAUL_` namespace that are not configuration keys.
const RESERVED_ENV: &[&str] = &[
    crate::ENV_SESSION,
    crate::ENV_EVENT_DIR,
    crate::ENV_INNER_WRAPPER,
    crate::ENV_WRAPPER_FAIL,
    crate::ENV_EXEC_GUARD,
    crate::ENV_REAL_CARGO,
    crate::ENV_HOME,
];

#[derive(Debug, Clone)]
pub struct Explained {
    pub key: &'static str,
    pub value: String,
    pub origin: String,
    pub env: String,
    pub help: &'static str,
}

#[derive(Debug, Clone)]
pub struct Setting {
    pub value: Val,
    pub origin: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    settings: BTreeMap<&'static str, Setting>,
    pub repo_config: Option<PathBuf>,
}

fn parse_scalar(key: &Key, raw: &str, origin: &str) -> Result<Val, String> {
    let bad = |expect: &str| format!("invalid value {:?} for `{}` from {}: expected {}", raw, key.name, origin, expect);
    match key.kind {
        Kind::Str => Ok(Val::Str(raw.to_string())),
        Kind::Enum(allowed) => {
            if allowed.contains(&raw) {
                Ok(Val::Str(raw.to_string()))
            } else {
                Err(bad(&format!("one of {}", allowed.join(", "))))
            }
        }
        Kind::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "on" | "yes" => Ok(Val::Bool(true)),
            "false" | "0" | "off" | "no" => Ok(Val::Bool(false)),
            _ => Err(bad("a boolean")),
        },
        Kind::Int => raw.parse::<i64>().ok().filter(|v| *v >= 0).map(Val::Int).ok_or_else(|| bad("a non-negative integer")),
    }
}

fn from_toml(key: &Key, v: &toml::Value, origin: &str) -> Result<Val, String> {
    match (v, &key.kind) {
        (toml::Value::String(s), _) => parse_scalar(key, s, origin),
        (toml::Value::Boolean(b), Kind::Bool) => Ok(Val::Bool(*b)),
        (toml::Value::Integer(i), Kind::Int) if *i >= 0 => Ok(Val::Int(*i)),
        _ => Err(format!("invalid type for `{}` from {}", key.name, origin)),
    }
}

/// The single environment variable that sets `key`.
fn env_name(key: &Key) -> String {
    match key.env {
        Some(name) => name.to_string(),
        None => format!("HAUL_{}", key.name.replace(['.', '-'], "_").to_ascii_uppercase()),
    }
}

/// Nearest ancestor of `start` that contains `.haul/config.toml`.
pub fn find_repo_config(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let p = d.join(".haul").join("config.toml");
        if p.is_file() {
            return Some(p);
        }
        dir = d.parent();
    }
    None
}

impl Config {
    pub fn defaults() -> Config {
        let mut settings = BTreeMap::new();
        for k in KEYS {
            let value = parse_scalar(k, k.default, "default").expect("built-in defaults are valid");
            settings.insert(k.name, Setting { value, origin: "default".into() });
        }
        Config { settings, repo_config: None }
    }

    fn apply_file(&mut self, path: &Path) -> Result<(), String> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            // Absent, including below a path that is not a directory: an unusable state
            // directory must not turn into a configuration error (NFR-020).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound || !path.exists() => return Ok(()),
            Err(e) => return Err(format!("cannot read {}: {}", path.display(), e)),
        };
        let origin = path.display().to_string();
        let doc: toml::Table = text.parse().map_err(|e| format!("invalid TOML in {}: {}", origin, e))?;
        for (section, body) in &doc {
            let table = body.as_table().ok_or_else(|| format!("`{}` in {} must be a table", section, origin))?;
            for (name, value) in table {
                let full = format!("{}.{}", section, name);
                // Never guess at a misspelt key: name the field and its source (error table).
                let key = KEYS.iter().find(|k| k.name == full).ok_or_else(|| format!("unknown configuration key `{}` in {}", full, origin))?;
                let v = from_toml(key, value, &origin)?;
                self.settings.insert(key.name, Setting { value: v, origin: origin.clone() });
            }
        }
        Ok(())
    }

    fn apply_env(&mut self) -> Result<(), String> {
        for k in KEYS {
            let name = env_name(k);
            if let Ok(raw) = std::env::var(&name) {
                if raw.is_empty() {
                    continue;
                }
                let origin = format!("env {}", name);
                let v = parse_scalar(k, &raw, &origin)?;
                self.settings.insert(k.name, Setting { value: v, origin });
            }
        }
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("HAUL_") && !RESERVED_ENV.contains(&name.as_ref()) && !KEYS.iter().any(|k| env_name(k) == name) {
                return Err(format!("unknown configuration variable `{}`", name));
            }
        }
        Ok(())
    }

    /// Loads the effective configuration for a command started in `cwd`.
    pub fn load(cwd: &Path) -> Result<Config, String> {
        let mut c = Config::defaults();
        c.apply_file(&crate::util::haul_home().join("config.toml"))?;
        if let Some(repo) = find_repo_config(cwd) {
            c.apply_file(&repo)?;
            c.repo_config = Some(repo);
        }
        c.apply_env()?;
        Ok(c)
    }

    /// Applies an explicit management flag (`--set section.key=value`), the highest precedence.
    pub fn set_flag(&mut self, assignment: &str) -> Result<(), String> {
        let (name, raw) = assignment.split_once('=').ok_or_else(|| format!("expected KEY=VALUE, got {:?}", assignment))?;
        let key = KEYS.iter().find(|k| k.name == name).ok_or_else(|| format!("unknown configuration key `{}`", name))?;
        let v = parse_scalar(key, raw, "flag")?;
        self.settings.insert(key.name, Setting { value: v, origin: "flag".into() });
        Ok(())
    }

    pub fn str(&self, key: &str) -> &str {
        match self.settings.get(key).map(|s| &s.value) {
            Some(Val::Str(s)) => s,
            _ => "",
        }
    }

    pub fn bool(&self, key: &str) -> bool {
        matches!(self.settings.get(key).map(|s| &s.value), Some(Val::Bool(true)))
    }

    pub fn int(&self, key: &str) -> u64 {
        match self.settings.get(key).map(|s| &s.value) {
            Some(Val::Int(i)) => *i as u64,
            _ => 0,
        }
    }

    pub fn origin(&self, key: &str) -> &str {
        self.settings.get(key).map(|s| s.origin.as_str()).unwrap_or("default")
    }

    /// Every effective setting with its origin and description (CFG-001, CFG-002).
    pub fn explain(&self) -> Vec<Explained> {
        KEYS.iter()
            .map(|k| {
                let s = &self.settings[k.name];
                Explained { key: k.name, value: s.value.to_string(), origin: s.origin.clone(), env: env_name(k), help: k.help }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_conservative() {
        let c = Config::defaults();
        assert_eq!(c.str("cache.provider"), "inherit");
        assert_eq!(c.str("incremental.local-dev"), "cargo-default");
        assert!(c.bool("measurement.enabled"));
        assert_eq!(c.origin("cache.fail"), "default");
    }

    #[test]
    fn rejects_unknown_keys_and_bad_values() {
        let dir = std::env::temp_dir().join(format!("haul-cfg-{}", crate::util::new_id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        fs::write(&p, "[cache]\nprovidr = \"sccache\"\n").unwrap();
        let err = Config::defaults().apply_file(&p).unwrap_err();
        assert!(err.contains("cache.providr") && err.contains("config.toml"), "{}", err);
        fs::write(&p, "[cache]\nfail = \"maybe\"\n").unwrap();
        assert!(Config::defaults().apply_file(&p).unwrap_err().contains("one of open, closed"));
        fs::write(&p, "[cache]\nprovider = \"sccache\"\n[measurement]\nkeep-runs = 7\n").unwrap();
        let mut c = Config::defaults();
        c.apply_file(&p).unwrap();
        assert_eq!(c.str("cache.provider"), "sccache");
        assert_eq!(c.int("measurement.keep-runs"), 7);
        assert!(c.origin("cache.provider").ends_with("config.toml"));
        c.set_flag("cache.provider=none").unwrap();
        assert_eq!(c.origin("cache.provider"), "flag");
        fs::remove_dir_all(&dir).ok();
    }
}
