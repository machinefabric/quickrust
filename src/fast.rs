//! Fast mode: build settings QuickRust applies by default (spec revision of 2026-10-11).
//!
//! Every setting here keeps the built program's behaviour identical; each one has to
//! earn its place in the speed index (`benchmarks/`). Settings reach the delegated Cargo
//! as `--config` overlays before the subcommand, never through files or the environment,
//! so they cannot leak into build scripts, programs run by `cargo run`, or nested Cargo.
//! A setting the user made anywhere (manifest, configuration, environment, `--config`)
//! always wins: QuickRust then leaves that setting, and the ones that depend on it, alone.

use crate::resolve::{Family, Invocation};
use serde_json::{json, Value};

pub struct Setting {
    /// Path below `[profile]`, as written in a `--config` assignment.
    pub key: &'static str,
    pub value: &'static str,
    /// User settings that make QuickRust leave this one alone, as flattened paths below `[profile]`.
    pub yields_to: &'static [&'static str],
    pub why: &'static str,
}

pub const SETTINGS: &[Setting] = &[
    Setting {
        key: "dev.debug",
        value: "\"line-tables-only\"",
        yields_to: &["dev.debug"],
        why: "line tables keep file:line in backtraces and debuggers; full variable and type information is what makes debug builds slow to generate, link and store",
    },
    Setting {
        key: "dev.package.\"*\".debug",
        value: "false",
        yields_to: &["dev.debug", "dev.package.*.debug"],
        why: "no debug information for dependencies in development builds, as Cargo's own build-performance guide recommends",
    },
];

/// Environment profile keys are `name_key` in lower case; compare in that shape.
fn env_form(path: &str) -> String {
    path.replace(['.', '-'], "_").to_ascii_lowercase()
}

pub struct Overlays {
    /// Arguments to insert before the subcommand.
    pub args: Vec<String>,
    pub record: Value,
}

/// What fast mode adds to one invocation. `user_keys` are the user's profile settings with
/// their origins (configuration and `--config`, the workspace manifest, the environment).
pub fn overlays(inv: &Invocation, mode: &str, user_keys: &[(String, String)]) -> Overlays {
    let none = |why: &str| Overlays { args: Vec::new(), record: json!({ "mode": mode, "applied": [], "yielded": [], "note": why }) };
    if mode != "fast" {
        return none("compat mode: the build is exactly upstream Cargo's");
    }
    // Built-in build commands only: Cargo does not hand `--config` to external subcommands.
    if !inv.family.builds() || inv.family == Family::External {
        return none("not a build command");
    }
    let mut args = Vec::new();
    let (mut applied, mut yielded) = (Vec::new(), Vec::new());
    for s in SETTINGS {
        let user = user_keys.iter().find(|(k, origin)| s.yields_to.iter().any(|y| if origin.starts_with("env ") { env_form(y) == *k } else { y == k }));
        match user {
            Some((k, origin)) => yielded.push(json!({ "key": format!("profile.{}", s.key), "because": format!("profile.{} is set in {}", k, origin) })),
            None => {
                args.push("--config".to_string());
                args.push(format!("profile.{}={}", s.key, s.value));
                applied.push(json!({ "key": format!("profile.{}", s.key), "value": s.value, "why": s.why }));
            }
        }
    }
    Overlays { args, record: json!({ "mode": mode, "applied": applied, "yielded": yielded }) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::classify;
    use std::ffi::OsString;

    fn inv(args: &[&str]) -> Invocation {
        classify(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    fn keys(o: &Overlays) -> Vec<String> {
        o.args.iter().filter(|a| *a != "--config").cloned().collect()
    }

    // TEST0052: Fast mode adds the settings to build commands only
    #[test]
    fn test0052_fast_mode_adds_the_settings_to_build_commands_only() {
        let o = overlays(&inv(&["build"]), "fast", &[]);
        assert_eq!(keys(&o), vec!["profile.dev.debug=\"line-tables-only\"", "profile.dev.package.\"*\".debug=false"]);
        assert_eq!(o.args[0], "--config");
        for cmd in [&["metadata"][..], &["publish"], &["nextest", "run"], &["clean"]] {
            assert!(overlays(&inv(cmd), "fast", &[]).args.is_empty(), "{:?}", cmd);
        }
        assert!(overlays(&inv(&["test"]), "compat", &[]).args.is_empty());
    }

    // TEST0053: A user setting always wins
    #[test]
    fn test0053_a_user_setting_always_wins() {
        // The user wants debug info in dev builds: QuickRust touches neither setting.
        let o = overlays(&inv(&["build"]), "fast", &[("dev.debug".into(), "manifest /w/Cargo.toml".into())]);
        assert!(o.args.is_empty());
        assert_eq!(o.record["yielded"].as_array().unwrap().len(), 2);
        assert!(o.record["yielded"][0]["because"].as_str().unwrap().contains("/w/Cargo.toml"));
        // Only the dependency setting was made: the workspace one still applies.
        let o = overlays(&inv(&["build"]), "fast", &[("dev.package.*.debug".into(), "cargo config x".into())]);
        assert_eq!(keys(&o), vec!["profile.dev.debug=\"line-tables-only\""]);
        // Environment keys arrive as `dev_debug`.
        assert!(overlays(&inv(&["build"]), "fast", &[("dev_debug".into(), "env CARGO_PROFILE_DEV_DEBUG".into())]).args.is_empty());
        // Unrelated settings change nothing.
        assert_eq!(keys(&overlays(&inv(&["build"]), "fast", &[("dev.opt-level".into(), "manifest".into()), ("release.debug".into(), "manifest".into())])).len(), 2);
    }
}
