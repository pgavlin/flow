//! Formatting only: Criterion remains responsible for measurement and statistics.
use super::Failure;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    fs,
    path::Path,
};

pub(super) type Timings = BTreeMap<String, f64>; // Nanoseconds per iteration.

fn insert(timings: &mut Timings, id: &str, estimate: &Value, unit: &str) -> Result<(), String> {
    let scale = match unit {
        "ns" => 1.0,
        "us" | "µs" => 1_000.0,
        "ms" => 1_000_000.0,
        "s" => 1_000_000_000.0,
        _ => return Err(format!("{id}: unsupported timing unit {unit:?}")),
    };
    let ns = estimate.as_f64().unwrap_or(f64::NAN) * scale;
    if id.is_empty() || !ns.is_finite() || ns <= 0.0 {
        return Err(format!(
            "{id:?}: expected a nonempty ID and a positive finite time estimate"
        ));
    }
    if timings.insert(id.to_owned(), ns).is_some() {
        return Err(format!("duplicate benchmark ID {id:?}"));
    }
    Ok(())
}

pub(super) fn parse(input: &str) -> Result<Timings, String> {
    let mut timings = Timings::new();
    for record in serde_json::Deserializer::from_str(input).into_iter::<Value>() {
        let record = record.map_err(|error| error.to_string())?;
        if let Some(cases) = record
            .get("benchmarks")
            .filter(|_| record.get("reason").is_none())
        {
            // Repository snapshot: Criterion's slope is preferred, falling back
            // to the mean, exactly as cargo-criterion's `typical` statistic does.
            for case in cases.as_array().ok_or("expected a benchmarks array")? {
                let id = case["benchmark"]["full_id"]
                    .as_str()
                    .ok_or("missing benchmark.full_id")?;
                let estimates = &case["estimates"];
                let typical = if estimates["slope"].is_null() {
                    &estimates["mean"]
                } else {
                    &estimates["slope"]
                };
                insert(&mut timings, id, &typical["point_estimate"], "ns")?;
            }
        } else {
            match record["reason"].as_str() {
                Some("benchmark-complete") => {
                    let id = record["id"].as_str().ok_or("missing benchmark ID")?;
                    let typical = &record["typical"];
                    let unit = typical["unit"]
                        .as_str()
                        .ok_or_else(|| format!("{id}: missing typical.unit"))?;
                    insert(&mut timings, id, &typical["estimate"], unit)?;
                }
                Some(_) => {} // Group, build, or future informational messages.
                None => {
                    return Err(
                        "expected cargo-criterion messages or a saved timing.json snapshot".into(),
                    );
                }
            }
        }
    }
    if timings.is_empty() {
        return Err("no benchmark measurements found".into());
    }
    Ok(timings)
}

/// Equal-weight geometric mean of after/before ratios, evaluated in log space.
/// Nonpositive/nonfinite pairs cannot contribute a geometric ratio.
pub(super) fn geomean(pairs: impl IntoIterator<Item = (f64, f64)>) -> (usize, Option<f64>) {
    let mut sum = 0.0;
    let mut count = 0;
    for (before, after) in pairs {
        if before.is_finite() && after.is_finite() && before > 0.0 && after > 0.0 {
            sum += after.ln() - before.ln();
            count += 1;
        }
    }
    (
        count,
        (count > 0).then(|| (sum / count as f64).exp_m1() * 100.0),
    )
}

pub(super) fn geomean_labels(pairs: impl IntoIterator<Item = (f64, f64)>) -> (String, String) {
    let (count, change) = geomean(pairs);
    let value = change.map_or_else(
        || "n/a".into(),
        |v| format!("{:+.1}%", if v.abs() < 0.05 { 0.0 } else { v }),
    );
    (format!("geomean (n={count})"), value)
}

pub(super) fn table(before: &Timings, after: &Timings) -> String {
    let names: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    let mut rows = vec![[
        "Benchmark".to_owned(),
        "Before".to_owned(),
        "After".to_owned(),
        "Change".to_owned(),
    ]];
    for name in names {
        let b = before.get(name).copied();
        let a = after.get(name).copied();
        // Use the same time unit in both columns of each row.
        let max = b.unwrap_or(0.0).max(a.unwrap_or(0.0));
        let (scale, unit) = if max >= 1e9 {
            (1e9, "s")
        } else if max >= 1e6 {
            (1e6, "ms")
        } else if max >= 1e3 {
            (1e3, "µs")
        } else {
            (1.0, "ns")
        };
        let time = |value: Option<f64>| {
            value.map_or_else(|| "—".into(), |v| format!("{:.3} {unit}", v / scale))
        };
        let change = match (b, a) {
            (Some(b), Some(a)) => {
                let delta = (a / b - 1.0) * 100.0;
                if delta.is_finite() {
                    format!("{delta:+.1}%")
                } else {
                    "n/a".into()
                }
            }
            (None, _) => "added".into(),
            (_, None) => "removed".into(),
        };
        rows.push([name.clone(), time(b), time(a), change]);
    }
    let (label, change) = geomean_labels(
        before
            .iter()
            .filter_map(|(id, &b)| after.get(id).map(|&a| (b, a))),
    );
    rows.push([label, "—".into(), "—".into(), change]);
    let widths: [usize; 4] =
        std::array::from_fn(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap());
    let mut output = String::new();
    for (i, row) in rows.iter().enumerate() {
        if i == rows.len() - 1 {
            output.push('\n');
        }
        writeln!(
            output,
            "{:<w0$}  {:>w1$}  {:>w2$}  {:>w3$}",
            row[0],
            row[1],
            row[2],
            row[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3]
        )
        .unwrap();
        if i == 0 {
            writeln!(
                output,
                "{}  {}  {}  {}",
                "-".repeat(widths[0]),
                "-".repeat(widths[1]),
                "-".repeat(widths[2]),
                "-".repeat(widths[3])
            )
            .unwrap();
        }
    }
    output
}

pub(super) fn load(path: &Path) -> Result<Timings, String> {
    let input = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    parse(&input).map_err(|error| format!("{}: {error}", path.display()))
}

pub(super) fn compare_files(before: &Path, after: &Path) -> Result<String, Failure> {
    let compare = || Ok(table(&load(before)?, &load(after)?));
    compare().map_err(|message| Failure { code: 1, message })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn message(id: &str, estimate: Value, unit: &str) -> Value {
        json!({"reason":"benchmark-complete", "id":id, "typical":{"estimate":estimate, "unit":unit}, "change":{"change":"Improved"}})
    }
    #[test]
    fn reads_json_stream_and_ignores_non_benchmark_messages() {
        let input = format!(
            "{}\n{}\n{}\n",
            json!({"reason":"group-complete", "benchmarks":["one", "two"]}),
            message("one", json!(2.0), "µs"),
            message("two", json!(3.0), "ms")
        );
        let timings = parse(&input).unwrap();
        assert_eq!(timings["one"], 2000.0);
        assert_eq!(timings["two"], 3_000_000.0);
    }
    #[test]
    fn snapshot_prefers_slope_and_falls_back_to_mean() {
        let input = json!({"benchmarks":[
            {"benchmark":{"full_id":"slope"}, "estimates":{"slope":{"point_estimate":42}, "mean":{"point_estimate":99}}},
            {"benchmark":{"full_id":"mean"}, "estimates":{"slope":null, "mean":{"point_estimate":99}}}
        ]});
        let timings = parse(&input.to_string()).unwrap();
        assert_eq!(timings["slope"], 42.0);
        assert_eq!(timings["mean"], 99.0);
    }
    #[test]
    fn rejects_invalid_or_ambiguous_data() {
        for input in [
            "".into(),
            "{".into(),
            "{}".into(),
            json!({"reason":"group-complete"}).to_string(),
            json!({"benchmarks":{}}).to_string(),
            message("x", json!(0), "ns").to_string(),
            message("x", json!(-1), "ns").to_string(),
            message("x", Value::Null, "ns").to_string(),
            message("x", json!(1), "cycles").to_string(),
            message("", json!(1), "ns").to_string(),
        ] {
            assert!(parse(&input).is_err(), "{input}");
        }
        let m = message("same", json!(1), "ns");
        assert!(
            parse(&format!("{m}\n{m}"))
                .unwrap_err()
                .contains("duplicate")
        );
    }
    #[test]
    fn formats_changes_and_reports_added_and_removed_cases() {
        let before = Timings::from([("shared".into(), 2_000_000.0), ("removed".into(), 10.0)]);
        let after = Timings::from([("shared".into(), 1_000_000.0), ("added".into(), 1_000.0)]);
        let report = table(&before, &after);
        assert!(report.lines().any(|l| l.contains("shared")
            && l.contains("2.000 ms")
            && l.contains("1.000 ms")
            && l.contains("-50.0%")));
        assert!(
            report
                .lines()
                .any(|l| l.starts_with("added") && l.ends_with("added"))
        );
        assert!(
            report
                .lines()
                .any(|l| l.starts_with("removed") && l.ends_with("removed"))
        );
        assert!(!report.contains("Improved")); // Do not borrow unrelated run statistics.
        assert!(table(&after, &before).contains("+100.0%"));
    }
    #[test]
    fn compatible_formats_can_be_compared_without_changing_estimator() {
        let snapshot = json!({"benchmarks":[{"benchmark":{"full_id":"one"}, "estimates":{"mean":{"point_estimate":2000}}}]});
        let before = parse(&snapshot.to_string()).unwrap();
        let after = parse(&message("one", json!(1.0), "us").to_string()).unwrap();
        assert!(table(&before, &after).contains("-50.0%"));
    }
    #[test]
    fn geomean_uses_equal_weight_ratios_including_unchanged_pairs() {
        let (n, change) = geomean([(1.0, 0.5), (1_000_000.0, 1_000_000.0)]);
        assert_eq!(n, 2);
        assert!((change.unwrap() - (0.5_f64.sqrt() - 1.0) * 100.0).abs() < 1e-10);
        let (_, change) = geomean([(1e-300, 1e300), (1e300, 1e-300)]);
        assert!(change.unwrap().abs() < 1e-10);
    }
    #[test]
    fn geomean_excludes_invalid_pairs_and_reports_no_data() {
        let pairs = [
            (0.0, 1.0),
            (1.0, 0.0),
            (-1.0, 1.0),
            (f64::INFINITY, 1.0),
            (1.0, f64::NAN),
            (4.0, 1.0),
        ];
        assert_eq!(geomean(pairs), (1, Some(-75.0)));
        assert_eq!(geomean([]), (0, None));
        assert_eq!(geomean_labels([]), ("geomean (n=0)".into(), "n/a".into()));
    }
    #[test]
    fn timing_summary_is_below_results_and_excludes_unmatched_cases() {
        let before = Timings::from([("same".into(), 1.0), ("removed".into(), 100.0)]);
        let after = Timings::from([("same".into(), 0.5), ("added".into(), 100.0)]);
        let report = table(&before, &after);
        let summary = report.lines().find(|l| l.starts_with("geomean")).unwrap();
        assert!(summary.contains("n=1") && summary.contains("-50.0%"));
        assert!(report.find(summary).unwrap() > report.find("same").unwrap());
    }
    #[test]
    fn missing_files_report_the_path() {
        let path = Path::new("flow-xtask-nonexistent-benchmark-input.json");
        let error = compare_files(path, path).unwrap_err();
        assert!(error.message.contains(path.to_str().unwrap()));
    }
}
