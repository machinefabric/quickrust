//! `qrust analyze <section> --filter KEY=VALUE … --sort FIELD[:asc|desc] --limit N`: the records
//! of a report section, narrowed and ordered before they are rendered, as text or JSON alike.
//!
//! A filter key is a field of the section's records (`package`, `kind`, `source`, `target`,
//! `features`, `freshness`, `artifact_cache`, `reason`, `category`, `confidence`, `rule`, …):
//! a record matches when the field equals the value, or, for a list field, contains it. Several
//! filters must all match. A key no record has is an error naming the fields there are.

use serde_json::Value;
use std::cmp::Ordering;
use std::collections::BTreeSet;

#[derive(Debug, Default)]
pub struct View {
    filters: Vec<(String, String)>,
    /// Field, and whether descending.
    sort: Option<(String, Option<bool>)>,
    limit: Option<usize>,
}

impl View {
    pub fn parse(filters: Vec<String>, sort: Option<String>, limit: Option<usize>) -> Result<View, String> {
        let filters = filters.into_iter().map(|f| f.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).ok_or_else(|| format!("--filter expects KEY=VALUE, got {:?}", f))).collect::<Result<_, _>>()?;
        let sort = match sort {
            None => None,
            Some(s) => Some(match s.rsplit_once(':') {
                Some((f, "asc")) => (f.to_string(), Some(false)),
                Some((f, "desc")) => (f.to_string(), Some(true)),
                Some((_, d)) => return Err(format!("--sort direction must be asc or desc, got {:?}", d)),
                None => (s, None),
            }),
        };
        if limit == Some(0) {
            return Err("--limit must be at least 1".into());
        }
        Ok(View { filters, sort, limit })
    }

    pub fn is_empty(&self) -> bool {
        self.filters.is_empty() && self.sort.is_none() && self.limit.is_none()
    }

    pub fn limit(&self) -> Option<usize> {
        self.limit
    }

    /// Narrows and orders the record lists of `section` in `report`.
    pub fn apply(&self, report: &mut Value, section: &str) -> Result<(), String> {
        let paths = lists(section).ok_or_else(|| format!("`qrust analyze {}` has no list of records to filter, sort or limit; sections that have: {}", section, SECTIONS_WITH_RECORDS.join(", ")))?;
        // The fields the section's records have, to reject a key none of them has.
        let mut fields: BTreeSet<String> = BTreeSet::new();
        for path in paths {
            visit(report, path, &mut |list| fields.extend(list.iter().filter_map(Value::as_object).flat_map(|o| o.keys().cloned())));
        }
        for key in self.filters.iter().map(|(k, _)| k).chain(self.sort.iter().map(|(f, _)| f)) {
            if !fields.contains(key) && !fields.is_empty() {
                return Err(format!("the records of `{}` have no field `{}`; they have: {}", section, key, fields.iter().cloned().collect::<Vec<_>>().join(", ")));
            }
        }
        for path in paths {
            let mut lists: Vec<&mut Vec<Value>> = Vec::new();
            collect(Some(&mut *report), path, &mut lists);
            for list in lists {
                list.retain(|r| self.filters.iter().all(|(k, v)| matches(&r[k.as_str()], v)));
                if let Some((field, desc)) = &self.sort {
                    list.sort_by(|a, b| order(&a[field.as_str()], &b[field.as_str()], *desc));
                }
                if let Some(n) = self.limit {
                    list.truncate(n);
                }
            }
        }
        Ok(())
    }
}

/// Sections with records, for messages.
const SECTIONS_WITH_RECORDS: [&str; 7] = ["timing", "rebuilds", "duplication", "scripts", "proc-macros", "storage", "recommendations"];

/// Where a section's record lists sit in the report.
fn lists(section: &str) -> Option<&'static [&'static [&'static str]]> {
    Some(match section {
        // The critical path is a chain, shown whole as the units' context.
        "timing" => &[&["units"]],
        "rebuilds" => &[&["rebuilds", "units"]],
        "duplication" => &[&["duplication", "versions"], &["duplication", "variants"]],
        "scripts" => &[&["build_scripts"]],
        "proc-macros" => &[&["proc_macros", "units"]],
        "storage" => &[&["artifacts", "*", "gc_plan", "candidates"]],
        "recommendations" => &[&["recommendations"]],
        _ => return None,
    })
}

/// Every list `path` names (`*` stands for each element of an array), read-only.
fn visit(at: &Value, path: &[&str], f: &mut dyn FnMut(&Vec<Value>)) {
    match path.split_first() {
        None => {
            if let Value::Array(a) = at {
                f(a);
            }
        }
        Some((&"*", rest)) => {
            for e in at.as_array().into_iter().flatten() {
                visit(e, rest, f);
            }
        }
        Some((key, rest)) => {
            if let Some(next) = at.get(*key) {
                visit(next, rest, f);
            }
        }
    }
}

/// Every list `path` names, to change.
fn collect<'a>(at: Option<&'a mut Value>, path: &[&str], out: &mut Vec<&'a mut Vec<Value>>) {
    let Some(at) = at else { return };
    match path.split_first() {
        None => {
            if let Value::Array(a) = at {
                out.push(a);
            }
        }
        Some((&"*", rest)) => {
            if let Value::Array(a) = at {
                for e in a.iter_mut() {
                    collect(Some(e), rest, out);
                }
            }
        }
        Some((key, rest)) => collect(at.get_mut(*key), rest, out),
    }
}

fn matches(field: &Value, want: &str) -> bool {
    match field {
        Value::String(s) => s == want,
        Value::Number(n) => n.to_string() == want || (want.parse::<f64>().ok().zip(n.as_f64()).is_some_and(|(w, n)| w == n)),
        Value::Bool(b) => want.parse::<bool>().ok() == Some(*b),
        Value::Array(a) => a.iter().any(|x| matches(x, want)),
        _ => false,
    }
}

/// Numbers largest first and text A to Z unless a direction is given; records without the
/// field last either way.
fn order(a: &Value, b: &Value, desc: Option<bool>) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Greater,
        (_, Value::Null) => Ordering::Less,
        (Value::Number(x), Value::Number(y)) => {
            let o = x.as_f64().partial_cmp(&y.as_f64()).unwrap_or(Ordering::Equal);
            if desc.unwrap_or(true) { o.reverse() } else { o }
        }
        _ => {
            let (x, y) = (text(a), text(b));
            let o = x.cmp(&y);
            if desc.unwrap_or(false) { o.reverse() } else { o }
        }
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report() -> Value {
        json!({
            "units": [
                { "package": "serde", "kind": "lib", "source": "registry", "duration_ns": 300, "features": ["std", "derive"], "freshness": "compiled" },
                { "package": "app", "kind": "bin", "source": "path", "duration_ns": 900, "features": [], "freshness": "restored" },
                { "package": "syn", "kind": "lib", "source": "registry", "duration_ns": 600, "features": ["full"], "freshness": "compiled" },
                { "package": "quote", "kind": "lib", "source": "registry", "features": [], "freshness": "fresh" },
            ],
            "critical_path": [{ "package": "app", "duration_ns": 900 }],
            "artifacts": [{ "gc_plan": { "candidates": [{ "group": "a", "category": "dependency", "allocated_bytes": 5 }, { "group": "b", "category": "incremental", "allocated_bytes": 9 }] } }],
        })
    }
    fn packages(v: &Value) -> Vec<&str> {
        v["units"].as_array().unwrap().iter().map(|u| u["package"].as_str().unwrap()).collect()
    }

    #[test]
    fn filters_all_match_and_list_fields_match_by_membership() {
        let mut r = report();
        View::parse(vec!["kind=lib".into(), "source=registry".into(), "features=full".into()], None, None).unwrap().apply(&mut r, "timing").unwrap();
        assert_eq!(packages(&r), ["syn"]);
        assert_eq!(r["critical_path"].as_array().unwrap().len(), 1, "the critical path is context, never cut");
    }

    #[test]
    fn numbers_sort_largest_first_text_from_a_and_records_without_the_field_last() {
        let mut r = report();
        View::parse(vec![], Some("duration_ns".into()), Some(2)).unwrap().apply(&mut r, "timing").unwrap();
        assert_eq!(packages(&r), ["app", "syn"]);
        let mut r = report();
        View::parse(vec![], Some("duration_ns:asc".into()), None).unwrap().apply(&mut r, "timing").unwrap();
        assert_eq!(packages(&r), ["serde", "syn", "app", "quote"]);
        let mut r = report();
        View::parse(vec![], Some("package".into()), None).unwrap().apply(&mut r, "timing").unwrap();
        assert_eq!(packages(&r), ["app", "quote", "serde", "syn"]);
    }

    #[test]
    fn storage_candidates_of_every_directory_are_records() {
        let mut r = report();
        View::parse(vec!["category=incremental".into()], None, None).unwrap().apply(&mut r, "storage").unwrap();
        assert_eq!(r["artifacts"][0]["gc_plan"]["candidates"], json!([{ "group": "b", "category": "incremental", "allocated_bytes": 9 }]));
    }

    #[test]
    fn unknown_fields_and_sections_without_records_are_errors() {
        let e = View::parse(vec!["profile=release".into()], None, None).unwrap().apply(&mut report(), "timing").unwrap_err();
        assert!(e.contains("no field `profile`") && e.contains("package"), "{}", e);
        assert!(View::parse(vec![], None, Some(3)).unwrap().apply(&mut report(), "cache").unwrap_err().contains("no list of records"));
        assert!(View::parse(vec!["kind".into()], None, None).is_err());
        assert!(View::parse(vec![], Some("x:sideways".into()), None).is_err());
        assert!(View::parse(vec![], None, Some(0)).is_err());
    }
}
