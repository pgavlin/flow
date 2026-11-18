use super::*;

fn temporary() -> Pending {
    Pending::new(&env::temp_dir().join("flow-perf-tests")).unwrap()
}
fn fixture(directory: &Path) {
    fs::create_dir_all(directory).unwrap();
    write(&directory.join("timing.jsonl"), &format!("{}\n", json!({"reason":"benchmark-complete", "id":"one", "typical":{"estimate":10,"unit":"ns"}}))).unwrap();
    let mut sample = json!({"benchmark":"one"});
    for field in FIELDS {
        sample[field] = json!(100);
    }
    let policy = json!({"cache_bytes":4096});
    let report = json!({"schema_version":1, "publication_font_context":policy, "samples":[sample]});
    for file in ["memory.json", "memory-2.json", "memory-3.json"] {
        write(&directory.join(file), &report.to_string()).unwrap();
    }
    write(&directory.join("metadata.json"), &json!({"schema_version":1,"complete":true,"benchmark_count":1,"memory":{"runs":3,"publication_font_context":policy}}).to_string()).unwrap();
    write(&directory.join("README.md"), "fixture readme\n").unwrap();
    write(&directory.join("comparison.txt"), "fixture comparison\n").unwrap();
}
fn old_baseline(root: &Path) -> PathBuf {
    let baseline = root.join(BASELINE);
    fs::create_dir_all(&baseline).unwrap();
    write(&baseline.join("old-sentinel"), "previous baseline").unwrap();
    baseline
}

#[test]
fn accepts_complete_capture_without_running_commands_and_removes_obsolete_files() {
    let root = temporary();
    let latest = root.0.join(LATEST);
    fixture(&latest);
    let baseline = old_baseline(&root.0);
    accept(&root.0).unwrap();
    assert!(!baseline.join("old-sentinel").exists());
    for name in FILES {
        assert_eq!(
            read(&latest.join(name)).unwrap(),
            read(&baseline.join(name)).unwrap()
        );
    }
}

#[test]
fn missing_incomplete_and_corrupt_captures_preserve_the_baseline() {
    let root = temporary();
    let baseline = old_baseline(&root.0);
    assert!(
        accept(&root.0)
            .unwrap_err()
            .contains("Run `cargo perf` first")
    );
    let latest = root.0.join(LATEST);
    fixture(&latest);
    let mut meta = json_file(&latest.join("metadata.json")).unwrap();
    meta["complete"] = json!(false);
    write(&latest.join("metadata.json"), &meta.to_string()).unwrap();
    assert!(accept(&root.0).is_err());
    fixture(&latest);
    fs::remove_file(latest.join("memory-3.json")).unwrap();
    assert!(accept(&root.0).is_err());
    fixture(&latest);
    write(&latest.join("timing.jsonl"), "{invalid").unwrap();
    assert!(accept(&root.0).is_err());
    fixture(&latest);
    fs::remove_file(latest.join("README.md")).unwrap();
    assert!(accept(&root.0).is_err());
    fixture(&latest);
    let mut meta = json_file(&latest.join("metadata.json")).unwrap();
    meta["benchmark_count"] = json!(2);
    write(&latest.join("metadata.json"), &meta.to_string()).unwrap();
    assert!(accept(&root.0).is_err());
    assert_eq!(
        read(&baseline.join("old-sentinel")).unwrap(),
        "previous baseline"
    );
}

#[test]
fn rejects_mismatched_workloads_duplicate_ids_and_policies() {
    let root = temporary();
    fixture(&root.0);
    let path = root.0.join("memory-2.json");
    let mut data = json_file(&path).unwrap();
    data["samples"][0]["benchmark"] = json!("other");
    write(&path, &data.to_string()).unwrap();
    assert!(Capture::load(&root.0).err().unwrap().contains("incomplete"));
    fixture(&root.0);
    let mut data = json_file(&path).unwrap();
    data["publication_font_context"]["cache_bytes"] = json!(42);
    write(&path, &data.to_string()).unwrap();
    assert!(Capture::load(&root.0).err().unwrap().contains("policy"));
    fixture(&root.0);
    let mut data = json_file(&path).unwrap();
    let duplicate = data["samples"][0].clone();
    data["samples"].as_array_mut().unwrap().push(duplicate);
    write(&path, &data.to_string()).unwrap();
    assert!(Capture::load(&root.0).err().unwrap().contains("duplicate"));
}

#[test]
fn medians_variation_ranges_and_changed_only_memory_report() {
    let before = temporary();
    fixture(&before.0);
    let after = temporary();
    fixture(&after.0);
    for (name, value) in [
        ("memory.json", 10),
        ("memory-2.json", 30),
        ("memory-3.json", 20),
    ] {
        let path = after.0.join(name);
        let mut data = json_file(&path).unwrap();
        data["samples"][0]["peak_live_bytes"] = json!(value);
        write(&path, &data.to_string()).unwrap();
    }
    let b = Capture::load(&before.0).unwrap();
    let a = Capture::load(&after.0).unwrap();
    assert_eq!(a.median("one", 2), Some(20));
    assert_eq!(
        a.variations(),
        vec![json!({"benchmark":"one","field":"peak_live_bytes","min":10,"max":30})]
    );
    let text = report(&b, &a);
    assert!(text.contains("-80.0%"));
    assert!(text.contains("peak_live_bytes"));
    assert!(
        !text
            .lines()
            .filter(|line| !line.starts_with("geomean"))
            .any(|line| line.contains("allocated_blocks"))
    );
    assert!(
        report(&b, &b)
            .lines()
            .filter(|line| line.starts_with("geomean"))
            .all(|line| line.ends_with("+0.0%"))
    );
}

#[test]
fn failed_directory_promotion_restores_previous_contents() {
    let root = temporary();
    let destination = root.0.join("latest");
    fs::create_dir(&destination).unwrap();
    write(&destination.join("sentinel"), "old").unwrap();
    assert!(replace_directory(&root.0.join("missing"), &destination).is_err());
    assert_eq!(read(&destination.join("sentinel")).unwrap(), "old");
}

#[test]
fn failed_child_and_abandoned_staging_leave_latest_untouched() {
    let root = temporary();
    let latest = root.0.join("latest");
    fs::create_dir(&latest).unwrap();
    write(&latest.join("sentinel"), "old").unwrap();
    let staged_path;
    {
        let pending = Pending::new(&latest).unwrap();
        staged_path = pending.0.clone();
        assert!(
            capture_command(
                &root.0,
                &["--flow-invalid-perf-option"],
                &pending.0.join("output")
            )
            .is_err()
        );
    }
    assert!(!staged_path.exists());
    assert_eq!(read(&latest.join("sentinel")).unwrap(), "old");
}

#[test]
fn memory_summaries_include_hidden_unchanged_cases_and_exclude_zeros() {
    let before = temporary();
    let after = temporary();
    fixture(&before.0);
    fixture(&after.0);
    let mut b = Capture::load(&before.0).unwrap();
    let mut a = Capture::load(&after.0).unwrap();
    for capture in [&mut b, &mut a] {
        capture.timings.insert("two".into(), 10.0);
        for run in &mut capture.memory {
            run.insert("two".into(), [100; 5]);
            for values in run.values_mut() {
                values[4] = 0;
            }
        }
    }
    for run in &mut a.memory {
        run.get_mut("one").unwrap()[2] = 25;
    }
    let text = report(&b, &a);
    let summaries: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("geomean"))
        .collect();
    assert_eq!(summaries.len(), 6); // Timing plus each independent memory metric.
    let peak = summaries
        .iter()
        .find(|line| line.contains("peak_live_bytes"))
        .unwrap();
    assert!(peak.contains("n=2") && peak.contains("-50.0%"));
    let blocks = summaries
        .iter()
        .find(|line| line.contains("allocated_blocks"))
        .unwrap();
    assert!(blocks.contains("n=2") && blocks.contains("+0.0%"));
    let dropped = summaries
        .iter()
        .find(|line| line.contains("live_bytes_after_output_drop"))
        .unwrap();
    assert!(dropped.contains("n=0") && dropped.contains("n/a"));
    assert!(text.rfind("geomean").unwrap() > text.rfind("one ").unwrap());
}

#[test]
fn compare_directories_includes_timing_and_memory() {
    let before = temporary();
    let after = temporary();
    fixture(&before.0);
    fixture(&after.0);
    let report = compare(&before.0, &after.0).unwrap();
    assert!(report.starts_with("Timing\n"));
    assert!(report.contains("\nMemory\n"));
    assert_eq!(
        report
            .lines()
            .filter(|line| line.starts_with("geomean"))
            .count(),
        6
    );
    assert!(!report.contains("WARNING"));
    assert!(!report.contains("statistical"));
    assert!(!report.contains("RSS"));
}

#[test]
fn legacy_snapshot_can_still_be_loaded() {
    let root = temporary();
    fixture(&root.0);
    fs::remove_file(root.0.join("timing.jsonl")).unwrap();
    write(&root.0.join("timing.json"),&json!({"benchmarks":[{"benchmark":{"full_id":"one"},"estimates":{"mean":{"point_estimate":10}}}]}).to_string()).unwrap();
    assert_eq!(Capture::load(&root.0).unwrap().timings["one"], 10.0);
}
