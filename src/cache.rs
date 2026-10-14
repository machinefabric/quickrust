//! sccache adapter: detection, preflight and statistics (FR-100).
//!
//! Cache correctness is delegated to sccache itself; QuickRust only composes the
//! wrapper chain and reports what the cache did.

use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn is_sccache(wrapper: &str) -> bool {
    Path::new(wrapper).file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase().starts_with("sccache")).unwrap_or(false)
}

/// Raw `sccache --show-stats` snapshot; doubles as the reachability preflight.
pub fn stats(sccache: &str) -> Result<Value, String> {
    let out = Command::new(sccache)
        .args(["--show-stats", "--stats-format=json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {}", sccache, e))?;
    if !out.status.success() {
        return Err(format!("{} --show-stats failed: {}", sccache, String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("unreadable sccache statistics: {}", e))
}

fn sum_counts(v: &Value) -> u64 {
    v["counts"].as_object().map(|o| o.values().filter_map(Value::as_u64).sum()).unwrap_or(0)
}

fn dur_ns(v: &Value) -> u64 {
    v["secs"].as_u64().unwrap_or(0) * 1_000_000_000 + v["nanos"].as_u64().unwrap_or(0)
}

/// Difference between two snapshots, in the report's `cache` shape.
/// Counters are server-wide: concurrent builds on the same server are included,
/// which the `scope` field states explicitly.
pub fn delta(before: &Value, after: &Value) -> Value {
    let (b, a) = (&before["stats"], &after["stats"]);
    let d = |f: &dyn Fn(&Value) -> u64| f(a).saturating_sub(f(b));
    let hits = d(&|s| sum_counts(&s["cache_hits"]));
    let misses = d(&|s| sum_counts(&s["cache_misses"]));
    let errors = d(&|s| sum_counts(&s["cache_errors"]) + s["cache_read_errors"].as_u64().unwrap_or(0) + s["cache_write_errors"].as_u64().unwrap_or(0));
    let timeouts = d(&|s| s["cache_timeouts"].as_u64().unwrap_or(0));
    let non_cacheable = d(&|s| s["requests_not_cacheable"].as_u64().unwrap_or(0) + s["non_cacheable_compilations"].as_u64().unwrap_or(0));
    let requests = d(&|s| s["compile_requests"].as_u64().unwrap_or(0));
    let read_ns = d(&|s| dur_ns(&s["cache_read_hit_duration"]));
    let write_ns = d(&|s| dur_ns(&s["cache_write_duration"]));
    let mut reasons = serde_json::Map::new();
    if let Some(o) = a["not_cached"].as_object() {
        for (k, v) in o {
            let n = v.as_u64().unwrap_or(0).saturating_sub(b["not_cached"][k].as_u64().unwrap_or(0));
            if n > 0 {
                reasons.insert(k.clone(), json!(n));
            }
        }
    }
    let size = |v: &Value| v["cache_size"].as_u64();
    let write_bytes = match (size(before), size(after)) {
        (Some(x), Some(y)) if y >= x => json!(y - x),
        _ => Value::Null,
    };
    json!({
        "provider": "sccache",
        "version": after["version"],
        "location": after["cache_location"],
        "scope": "sccache-server counters between session start and end (includes any concurrent builds)",
        "requests": requests,
        "cacheable_requests": hits + misses,
        "hits": hits,
        "misses": misses,
        "non_cacheable": non_cacheable,
        "non_cacheable_reasons": reasons,
        "errors": errors,
        "timeouts": timeouts,
        "writes": d(&|s| s["cache_writes"].as_u64().unwrap_or(0)),
        "read_bytes": null,
        "write_bytes": write_bytes,
        "lookup_time_ns": read_ns,
        "write_time_ns": write_ns,
        "compile_time_on_miss_ns": d(&|s| dur_ns(&s["compiler_write_duration"])),
        "cache_size_bytes": after["cache_size"],
        "max_cache_size_bytes": after["max_cache_size"],
        "unavailable": {
            "read_bytes": "sccache does not report bytes restored from the cache",
            "write_bytes": if write_bytes.is_null() { json!("sccache did not report a cache size for both snapshots") } else { json!(null) },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(hits: u64, misses: u64, nc: u64, req: u64) -> Value {
        json!({ "stats": {
            "compile_requests": req, "requests_not_cacheable": nc, "non_cacheable_compilations": 0,
            "cache_hits": { "counts": { "Rust": hits } }, "cache_misses": { "counts": { "Rust": misses } },
            "cache_errors": { "counts": {} }, "cache_timeouts": 0, "cache_read_errors": 0, "cache_write_errors": 0,
            "cache_writes": misses, "cache_write_duration": { "secs": 0, "nanos": 5 },
            "cache_read_hit_duration": { "secs": 1, "nanos": 0 }, "compiler_write_duration": { "secs": 0, "nanos": 0 },
            "not_cached": { "incremental": nc } }, "cache_size": 100 + misses, "version": "0.10.0" })
    }

    // TEST0033: Hit rate keeps its request count
    #[test]
    fn test0033_hit_rate_keeps_its_request_count() {
        // FR-100 acceptance: 100 cacheable requests with 80 hits is reported as 80/100.
        let d = delta(&snap(10, 5, 1, 20), &snap(90, 25, 4, 130));
        assert_eq!(d["hits"], 80);
        assert_eq!(d["misses"], 20);
        assert_eq!(d["cacheable_requests"], 100);
        assert_eq!(d["non_cacheable"], 3);
        assert_eq!(d["non_cacheable_reasons"]["incremental"], 3);
        assert_eq!(d["requests"], 110);
        assert!(d["read_bytes"].is_null());
        assert_eq!(d["write_bytes"], 20);
    }

    // TEST0034: Recognises sccache
    #[test]
    fn test0034_recognises_sccache() {
        assert!(is_sccache("/usr/local/bin/sccache"));
        assert!(is_sccache("sccache.exe"));
        assert!(!is_sccache("/opt/bin/my-wrapper"));
    }
}
