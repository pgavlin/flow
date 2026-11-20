//! Serial benchmark capture and explicit baseline acceptance; no measurements in accept mode.
use super::{Failure, Tool, perf};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, String>;
type Counters = BTreeMap<String, [u64; 5]>;
const BASELINE: &str = "docs/performance-baseline";
const LATEST: &str = "target/performance/latest";
const FIELDS: [&str; 5] = [
    "allocated_bytes",
    "allocated_blocks",
    "peak_live_bytes",
    "live_bytes_at_completion",
    "live_bytes_after_output_drop",
];
const FILES: [&str; 7] = [
    "timing.jsonl",
    "memory.json",
    "memory-2.json",
    "memory-3.json",
    "metadata.json",
    "README.md",
    "comparison.txt",
];
const TIMING_ARGS: &[&str] = &[
    "criterion",
    "--locked",
    "-p",
    "flow_publication",
    "--features",
    "render",
    "--bench",
    "opening",
    "--message-format=json",
    "--plotting-backend",
    "disabled",
];
const MEMORY_ARGS: &[&str] = &[
    "bench",
    "--locked",
    "-p",
    "flow_publication",
    "--features",
    "render",
    "--bench",
    "memory",
];

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}
fn write(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}
fn json_file(path: &Path) -> Result<Value> {
    serde_json::from_str(&read(path)?).map_err(|e| format!("{}: {e}", path.display()))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn unique(parent: &Path, label: &str) -> PathBuf {
    parent.join(format!(
        ".{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

// Staging directories are on the destination filesystem and removed on ordinary errors.
struct Pending(PathBuf);
impl Pending {
    fn new(destination: &Path) -> Result<Self> {
        let parent = destination.parent().ok_or("destination has no parent")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        loop {
            let path = unique(parent, "perf-pending");
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn replace_directory(staged: &Path, destination: &Path) -> Result<()> {
    let backup = unique(destination.parent().unwrap(), "perf-previous");
    let existed = destination.exists();
    if existed {
        fs::rename(destination, &backup)
            .map_err(|e| format!("could not preserve {}: {e}", destination.display()))?;
    }
    if let Err(error) = fs::rename(staged, destination) {
        if existed {
            fs::rename(&backup, destination).map_err(|restore| format!("replacement failed ({error}); restore failed ({restore}); previous data remains at {}", backup.display()))?;
        }
        return Err(format!(
            "could not replace {}: {error}",
            destination.display()
        ));
    }
    if existed {
        fs::remove_dir_all(&backup).map_err(|e| {
            format!(
                "replacement succeeded, but could not remove {}: {e}",
                backup.display()
            )
        })?;
    }
    Ok(())
}

struct Capture {
    timings: perf::Timings,
    memory: Vec<Counters>,
    policy: Value,
}
impl Capture {
    fn load(directory: &Path) -> Result<Self> {
        let timing = if directory.join("timing.jsonl").is_file() {
            "timing.jsonl"
        } else {
            "timing.json"
        };
        let timings = perf::load(&directory.join(timing))?;
        let mut memory = Vec::new();
        let mut policy = Value::Null;
        for file in ["memory.json", "memory-2.json", "memory-3.json"] {
            let report = json_file(&directory.join(file))?;
            if report["schema_version"] != 1 {
                return Err(format!("{file}: unsupported memory schema"));
            }
            if memory.is_empty() {
                policy = report["publication_font_context"].clone();
            }
            if policy.is_null() || report["publication_font_context"] != policy {
                return Err(format!("{file}: missing or inconsistent font cache policy"));
            }
            let mut counters = Counters::new();
            for sample in report["samples"]
                .as_array()
                .ok_or_else(|| format!("{file}: missing samples array"))?
            {
                let id = sample["benchmark"]
                    .as_str()
                    .ok_or("missing memory benchmark ID")?;
                let mut values = [0; 5];
                for (i, field) in FIELDS.iter().enumerate() {
                    values[i] = sample[*field]
                        .as_u64()
                        .ok_or_else(|| format!("{file}: {id}: invalid {field}"))?;
                }
                if counters.insert(id.to_owned(), values).is_some() {
                    return Err(format!("{file}: duplicate benchmark {id}"));
                }
            }
            if !timings.keys().eq(counters.keys()) {
                return Err(format!(
                    "{file}: timing and memory workloads differ; incomplete capture"
                ));
            }
            memory.push(counters);
        }
        Ok(Self {
            timings,
            memory,
            policy,
        })
    }
    fn median(&self, id: &str, field: usize) -> Option<u64> {
        let mut values: Vec<_> = self
            .memory
            .iter()
            .filter_map(|run| run.get(id).map(|v| v[field]))
            .collect();
        values.sort_unstable();
        values.get(values.len() / 2).copied()
    }
    fn variations(&self) -> Vec<Value> {
        let mut rows = Vec::new();
        for id in self.timings.keys() {
            for (i, field) in FIELDS.iter().enumerate() {
                let min = self.memory.iter().map(|run| run[id][i]).min().unwrap();
                let max = self.memory.iter().map(|run| run[id][i]).max().unwrap();
                if min != max {
                    rows.push(json!({"benchmark":id, "field":field, "min":min, "max":max}));
                }
            }
        }
        rows
    }
}

fn report(before: &Capture, after: &Capture) -> String {
    let mut result = format!(
        "Timing\n\n{}\nMemory\n\n",
        perf::table(&before.timings, &after.timings)
    );
    let names: BTreeSet<_> = before.timings.keys().chain(after.timings.keys()).collect();
    let width = names
        .iter()
        .map(|s| s.len())
        .max()
        .unwrap()
        .max(format!("geomean (n={})", names.len()).len());
    let header = format!(
        "{:<width$}  {:<28}  {:>14}  {:>14}  {:>8}\n",
        "Benchmark", "Metric", "Before", "After", "Change"
    );
    let mut rows = String::new();
    for name in names {
        for (i, field) in FIELDS.iter().enumerate() {
            let b = before.median(name, i);
            let a = after.median(name, i);
            if b == a {
                continue;
            }
            let value = |v: Option<u64>| {
                v.map_or_else(
                    || "—".into(),
                    |n| {
                        if i == 1 {
                            n.to_string()
                        } else {
                            format!("{:.3} MiB", n as f64 / 1_048_576.0)
                        }
                    },
                )
            };
            let change = match (b, a) {
                (Some(0), Some(_)) => "from zero".into(),
                (Some(b), Some(a)) => format!("{:+.1}%", (a as f64 / b as f64 - 1.0) * 100.0),
                (None, _) => "added".into(),
                (_, None) => "removed".into(),
            };
            rows.push_str(&format!(
                "{name:<width$}  {field:<28}  {:>14}  {:>14}  {change:>8}\n",
                value(b),
                value(a)
            ));
        }
    }
    result.push_str(&header);
    result.push_str(&rows);
    if !rows.is_empty() {
        result.push('\n');
    }
    for (i, field) in FIELDS.iter().enumerate() {
        let (label, change) =
            perf::geomean_labels(before.timings.keys().filter_map(|id| {
                Some((before.median(id, i)? as f64, after.median(id, i)? as f64))
            }));
        result.push_str(&format!(
            "{label:<width$}  {field:<28}  {:>14}  {:>14}  {change:>8}\n",
            "—", "—"
        ));
    }
    result
}

fn output(root: &Path, tool: &std::ffi::OsStr, args: &[&str]) -> Result<String> {
    let out = Command::new(tool)
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
fn capture_command(root: &Path, args: &[&str], destination: &Path) -> Result<()> {
    eprintln!("\n==> cargo {}", args.join(" "));
    let file = fs::File::create(destination).map_err(|e| e.to_string())?;
    let output = Command::new(Tool::Cargo.executable())
        .current_dir(root)
        .args(args)
        .stdout(file)
        .output()
        .map_err(|e| format!("could not run benchmark: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "benchmark command failed ({}); previous capture was not replaced\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
fn host(root: &Path) -> Value {
    let optional = |program: &str, args: &[&str]| output(root, program.as_ref(), args).ok();
    json!({"os":env::consts::OS, "architecture":env::consts::ARCH,
        "os_version":optional("sw_vers", &["-productVersion"]).or_else(|| optional("uname", &["-r"])),
        "cpu":optional("sysctl", &["-n", "machdep.cpu.brand_string"]).or_else(|| env::var("PROCESSOR_IDENTIFIER").ok()).or_else(|| fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| s.lines().find_map(|l| l.strip_prefix("model name").and_then(|l| l.split_once(':')).map(|(_,v)| v.trim().to_owned())))),
        "memory_bytes":optional("sysctl", &["-n", "hw.memsize"]).and_then(|s| s.parse::<u64>().ok()).or_else(|| fs::read_to_string("/proc/meminfo").ok().and_then(|s| s.lines().find_map(|l| l.strip_prefix("MemTotal:").and_then(|v| v.split_whitespace().next()).and_then(|v| v.parse::<u64>().ok()).map(|kb| kb * 1024))))})
}

fn capture(root: &Path) -> Result<()> {
    let baseline = Capture::load(&root.join(BASELINE))?;
    let cargo = Tool::Cargo.executable();
    let version = output(root, &cargo, &["criterion", "--version"]).map_err(|e| {
        format!("{e}\nInstall: cargo install --locked cargo-criterion --version 1.1.0")
    })?;
    let latest = root.join(LATEST);
    let pending = Pending::new(&latest)?;
    let started = now();
    let environment: BTreeMap<_, _> = env::vars()
        .filter(|(k, _)| {
            k.starts_with("FLOW_BENCH_")
                || k.starts_with("CARGO_PROFILE_BENCH_")
                || [
                    "RUSTFLAGS",
                    "CARGO_ENCODED_RUSTFLAGS",
                    "CARGO_BUILD_TARGET",
                    "CARGO_TARGET_DIR",
                    "RUSTUP_TOOLCHAIN",
                ]
                .contains(&k.as_str())
        })
        .collect();
    let mut metadata = json!({"schema_version":1, "complete":false, "features":["render"], "profile":"bench", "started_unix_seconds":started, "host":host(root), "environment":environment,
        "toolchain":{"cargo":output(root, &cargo, &["--version"])? , "rustc":output(root, "rustc".as_ref(), &["-Vv"])? , "cargo_criterion":version},
        "timing":{"command":TIMING_ARGS}, "memory":{"command":MEMORY_ARGS, "runs":3}});
    capture_command(root, TIMING_ARGS, &pending.0.join("timing.jsonl"))?;
    for file in ["memory.json", "memory-2.json", "memory-3.json"] {
        capture_command(root, MEMORY_ARGS, &pending.0.join(file))?;
    }
    let measured = Capture::load(&pending.0)?;
    let comparison = report(&baseline, &measured);
    metadata["complete"] = json!(true);
    metadata["finished_unix_seconds"] = json!(now());
    metadata["benchmark_count"] = json!(measured.timings.len());
    metadata["memory"]["publication_font_context"] = measured.policy.clone();
    metadata["memory"]["variation_ranges"] = json!(measured.variations());
    write(
        &pending.0.join("metadata.json"),
        &format!("{}\n", serde_json::to_string_pretty(&metadata).unwrap()),
    )?;
    write(&pending.0.join("comparison.txt"), &comparison)?;
    write(
        &pending.0.join("README.md"),
        "# Performance baseline\n\nCaptured by `cargo perf`: full Criterion timing followed by three serial DHAT\nruns. See [measurement guidance](../performance.md) and `metadata.json` for\nthe environment, cache policy, commands, timestamps, and variation ranges.\n\n- `timing.jsonl`: raw cargo-criterion messages, samples and confidence intervals.\n- `memory.json`, `memory-2.json`, `memory-3.json`: raw allocation reports.\n- `comparison.txt`: comparison against the baseline present at capture time,\n  using typical timings and median memory counters across three runs.\n\nUpdate this directory with `cargo perf --accept` in performance-affecting commits.\nGit history holds previous baselines; filenames do not contain revision IDs.\n",
    )?;
    replace_directory(&pending.0, &latest)?;
    print!("{comparison}");
    eprintln!(
        "\nSaved complete capture to {}. Use `cargo perf --accept` to update the baseline.",
        latest.display()
    );
    Ok(())
}

fn accept(root: &Path) -> Result<()> {
    let latest = root.join(LATEST);
    let metadata = json_file(&latest.join("metadata.json"))
        .map_err(|e| format!("{e}\nRun `cargo perf` first."))?;
    if metadata["schema_version"] != 1
        || metadata["complete"] != true
        || metadata["memory"]["runs"] != 3
    {
        return Err(
            "latest capture is not a complete full-suite run; run `cargo perf` first".into(),
        );
    }
    let data = Capture::load(&latest)?;
    if metadata["benchmark_count"].as_u64() != Some(data.timings.len() as u64)
        || metadata["memory"]["publication_font_context"] != data.policy
    {
        return Err("capture metadata does not match the measurements".into());
    }
    let destination = root.join(BASELINE);
    let pending = Pending::new(&destination)?;
    for name in FILES {
        fs::copy(latest.join(name), pending.0.join(name))
            .map_err(|e| format!("could not copy {name}: {e}"))?;
    }
    replace_directory(&pending.0, &destination)?;
    eprintln!(
        "Accepted {} into {}. No benchmarks were rerun; review and commit the baseline with the code.",
        latest.display(),
        destination.display()
    );
    Ok(())
}

pub(super) fn compare(before: &Path, after: &Path) -> std::result::Result<String, Failure> {
    if before.is_dir() && after.is_dir() {
        let compare = || Ok(report(&Capture::load(before)?, &Capture::load(after)?));
        compare().map_err(|message| Failure { code: 1, message })
    } else {
        perf::compare_files(before, after)
    }
}

pub(super) fn run(root: &Path, accepting: bool) -> std::result::Result<(), Failure> {
    if accepting {
        accept(root)
    } else {
        capture(root)
    }
    .map_err(|message| Failure { code: 1, message })
}

#[cfg(test)]
mod tests;
