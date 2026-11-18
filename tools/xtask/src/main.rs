#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "Build tooling owns filesystem access and process execution"
)]
mod web;

use std::{
    env,
    ffi::OsString,
    fmt,
    path::Path,
    process::{self, Command},
};

const HELP: &str = "\
Usage: cargo lint [--all]
       cargo build-web [--dev]

Run from the repository root. Lint requires Clippy and rustfmt.

Default: formatting and all-target/all-feature Clippy.
--all:   also check minimal core configurations, the demo reader,
         and the lint runner's own tests.

build-web: assemble the browser demo (release by default; --dev for debug).
           Requires the wasm32-unknown-unknown target, WASM-capable clang/LLVM,
           and the matching wasm-bindgen CLI; see examples/reader/README.md.
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Help,
    Lint { all: bool },
    BuildWeb { dev: bool },
}

fn parse_args(args: &[String]) -> Result<Mode, Failure> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["lint"] => Ok(Mode::Lint { all: false }),
        ["lint", "--all"] => Ok(Mode::Lint { all: true }),
        ["build-web"] => Ok(Mode::BuildWeb { dev: false }),
        ["build-web", "--dev"] => Ok(Mode::BuildWeb { dev: true }),
        ["lint" | "build-web", "--help" | "-h"] => Ok(Mode::Help),
        _ => Err(Failure {
            code: 2,
            message: format!("invalid arguments\n\n{HELP}"),
        }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Cargo,
    WasmBindgen,
}

impl Tool {
    fn executable(self) -> OsString {
        match self {
            Self::Cargo => env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
            Self::WasmBindgen => "wasm-bindgen".into(),
        }
    }
}

#[derive(Debug)]
struct Check {
    label: &'static str,
    tool: Tool,
    args: Vec<OsString>,
    forbidden_output: &'static [&'static str],
}

impl Check {
    fn new(label: &'static str, tool: Tool, args: &[&str]) -> Self {
        Self {
            label,
            tool,
            args: args.iter().map(OsString::from).collect(),
            forbidden_output: &[],
        }
    }

    fn reject_output(mut self, prefixes: &'static [&'static str]) -> Self {
        self.forbidden_output = prefixes;
        self
    }

    fn validate_output(&self, output: &str) -> Result<(), Failure> {
        let violations: Vec<_> = output
            .lines()
            .filter(|line| {
                self.forbidden_output
                    .iter()
                    .any(|prefix| line.starts_with(prefix))
            })
            .collect();
        if violations.is_empty() {
            return Ok(());
        }
        Err(Failure {
            code: 1,
            message: format!(
                "{} failed: forbidden dependencies/features:\n{}",
                self.label,
                violations.join("\n")
            ),
        })
    }

    fn run(&self, root: &Path) -> Result<(), Failure> {
        eprintln!("\n==> {}", self.label);
        let mut command = Command::new(self.tool.executable());
        command.current_dir(root).args(&self.args);
        if self.forbidden_output.is_empty() {
            return run_command(&mut command, self.label);
        }
        let output = command
            .stderr(process::Stdio::inherit())
            .output()
            .map_err(|error| Failure {
                code: 1,
                message: format!("could not run {}: {error}", self.label),
            })?;
        if !output.status.success() {
            return Err(Failure {
                code: output.status.code().unwrap_or(1),
                message: format!("{} failed ({})", self.label, output.status),
            });
        }
        self.validate_output(&String::from_utf8_lossy(&output.stdout))
    }
}

#[derive(Debug)]
struct Failure {
    code: i32,
    message: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

fn run_command(command: &mut Command, label: &str) -> Result<(), Failure> {
    let status = command.status().map_err(|error| Failure {
        code: 1,
        message: format!("could not run {label}: {error}\nCommand: {command:?}"),
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(Failure {
            code: status.code().unwrap_or(1),
            message: format!("{label} failed ({status})"),
        })
    }
}

fn checks(all: bool) -> Vec<Check> {
    use Tool::Cargo;

    let mut checks = vec![
        Check::new(
            "Format engine workspace",
            Cargo,
            &["fmt", "--all", "--", "--check"],
        ),
        Check::new(
            "Format demo reader",
            Cargo,
            &[
                "fmt",
                "--manifest-path",
                "examples/reader/Cargo.toml",
                "--",
                "--check",
            ],
        ),
        Check::new(
            "Format lint runner",
            Cargo,
            &[
                "fmt",
                "--manifest-path",
                "tools/xtask/Cargo.toml",
                "--",
                "--check",
            ],
        ),
        Check::new(
            "Clippy: all targets and features",
            Cargo,
            &[
                "clippy",
                "--locked",
                "--workspace",
                "--all-targets",
                "--all-features",
            ],
        ),
        Check::new(
            "Clippy: lint runner",
            Cargo,
            &[
                "clippy",
                "--locked",
                "--manifest-path",
                "tools/xtask/Cargo.toml",
                "--all-targets",
            ],
        ),
    ];
    if all {
        checks.extend([
            Check::new(
                "Build document core without warnings",
                Cargo,
                &[
                    "rustc",
                    "--locked",
                    "-p",
                    "flow_document",
                    "--lib",
                    "--no-default-features",
                    "--",
                    "-D",
                    "warnings",
                ],
            ),
            Check::new(
                "Clippy: document core",
                Cargo,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "flow_document",
                    "--lib",
                    "--no-default-features",
                ],
            ),
            Check::new(
                "Clippy: publication core and codecs",
                Cargo,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "flow_publication",
                    "--lib",
                    "--no-default-features",
                ],
            ),
            Check::new(
                "Clippy: demo reader host",
                Cargo,
                &[
                    "clippy",
                    "--locked",
                    "--manifest-path",
                    "examples/reader/Cargo.toml",
                    "--all-targets",
                ],
            ),
            Check::new(
                "Demo reader host tests",
                Cargo,
                &[
                    "test",
                    "--locked",
                    "--manifest-path",
                    "examples/reader/Cargo.toml",
                ],
            ),
            Check::new(
                "Shared cache dependency boundary",
                Cargo,
                &[
                    "tree",
                    "--locked",
                    "--color",
                    "never",
                    "-p",
                    "flow_cache",
                    "--edges",
                    "normal",
                    "--prefix",
                    "none",
                ],
            )
            .reject_output(&[
                "flow_document v",
                "flow_publication v",
                "fontique v",
                "harfrust v",
                "skrifa v",
                "swash v",
                "image v",
                "tracing v",
            ]),
            Check::new(
                "Linux font dependency boundary",
                Cargo,
                &[
                    "tree",
                    "--locked",
                    "--color",
                    "never",
                    "--workspace",
                    "--all-features",
                    "--target",
                    "x86_64-unknown-linux-gnu",
                    "--edges",
                    "features",
                    "--prefix",
                    "none",
                ],
            )
            .reject_output(&["fontique feature \"system\"", "yeslogic-fontconfig-sys v"]),
            Check::new(
                "Lint runner tests",
                Cargo,
                &[
                    "test",
                    "--locked",
                    "--manifest-path",
                    "tools/xtask/Cargo.toml",
                ],
            ),
        ]);
    }
    checks
}

fn run() -> Result<(), Failure> {
    let args: Vec<_> = env::args().skip(1).collect();
    let mode = parse_args(&args)?;
    if mode == Mode::Help {
        print!("{HELP}");
        return Ok(());
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|error| Failure {
            code: 1,
            message: format!("could not locate the repository root: {error}"),
        })?;
    match mode {
        Mode::Lint { all } => {
            for check in checks(all) {
                check.run(&root)?;
            }
            eprintln!("\nLint checks passed.");
        }
        Mode::BuildWeb { dev } => web::build(&root, dev)?,
        Mode::Help => unreachable!(),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("xtask: {error}");
        process::exit(error.code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn modes_and_help() {
        assert_eq!(
            parse_args(&args(&["lint"])).unwrap(),
            Mode::Lint { all: false }
        );
        assert_eq!(
            parse_args(&args(&["lint", "--all"])).unwrap(),
            Mode::Lint { all: true }
        );
        assert_eq!(
            parse_args(&args(&["build-web"])).unwrap(),
            Mode::BuildWeb { dev: false }
        );
        assert_eq!(
            parse_args(&args(&["build-web", "--dev"])).unwrap(),
            Mode::BuildWeb { dev: true }
        );
        for command in ["lint", "build-web"] {
            for flag in ["-h", "--help"] {
                assert_eq!(parse_args(&args(&[command, flag])).unwrap(), Mode::Help);
            }
        }
    }

    #[test]
    fn invalid_arguments_fail_before_running_checks() {
        for values in [
            vec![],
            vec!["lint", "--unknown"],
            vec!["lint", "--all", "extra"],
            vec!["build-web", "--all"],
            vec!["build-web", "--dev", "extra"],
        ] {
            assert_eq!(parse_args(&args(&values)).unwrap_err().code, 2);
        }
    }

    #[test]
    fn full_plan_extends_default_checks() {
        let basic = checks(false);
        let full = checks(true);
        assert_eq!(basic.len(), 5);
        assert_eq!(full.len(), 13);
        for (basic, full) in basic.iter().zip(&full) {
            assert_eq!(basic.label, full.label);
            assert_eq!(basic.tool, full.tool);
            assert_eq!(basic.args, full.args);
            assert_eq!(basic.forbidden_output, full.forbidden_output);
        }
    }

    #[test]
    fn dependency_checks_reject_forbidden_nodes_without_matching_similar_names() {
        let plan = checks(true);
        let cache = plan
            .iter()
            .find(|check| check.label == "Shared cache dependency boundary")
            .unwrap();
        cache
            .validate_output("flow_cache v0.1.0\nserde v1.0.0\nimageproc v0.1.0\n")
            .unwrap();
        for node in ["image v0.25.0", "fontique v0.11.1", "flow_document v0.1.0"] {
            assert!(
                cache
                    .validate_output(node)
                    .unwrap_err()
                    .message
                    .contains(node)
            );
        }
        let fonts = plan
            .iter()
            .find(|check| check.label == "Linux font dependency boundary")
            .unwrap();
        fonts
            .validate_output("fontique feature \"std\"\nfontique v0.11.1\n")
            .unwrap();
        for node in [
            "fontique feature \"system\"",
            "yeslogic-fontconfig-sys v6.0.1",
        ] {
            assert!(fonts.validate_output(node).is_err());
        }
        assert!(
            fonts
                .args
                .iter()
                .any(|arg| arg == "x86_64-unknown-linux-gnu")
        );
    }

    #[test]
    fn core_build_denies_compiler_warnings() {
        let plan = checks(true);
        let core = plan
            .iter()
            .find(|check| check.label == "Build document core without warnings")
            .unwrap();
        assert_eq!(core.tool, Tool::Cargo);
        assert_eq!(
            core.args,
            args(&[
                "rustc",
                "--locked",
                "-p",
                "flow_document",
                "--lib",
                "--no-default-features",
                "--",
                "-D",
                "warnings"
            ])
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn missing_program_is_an_error() {
        let error = run_command(
            &mut Command::new("flow-xtask-nonexistent-executable"),
            "missing tool",
        )
        .unwrap_err();
        assert_eq!(error.code, 1);
        assert!(error.message.contains("could not run missing tool"));
    }

    #[test]
    fn child_failure_is_propagated() {
        // Invoke this test harness with an invalid option: compile/run no fixture
        // application code and depend on no platform-specific shell executable.
        let error = run_command(
            Command::new(env::current_exe().unwrap())
                .arg("--flow-invalid-test-option")
                .stdout(process::Stdio::null())
                .stderr(process::Stdio::null()),
            "failing check",
        )
        .unwrap_err();
        assert_ne!(error.code, 0);
        assert!(error.message.contains("failing check failed"));
    }
}
