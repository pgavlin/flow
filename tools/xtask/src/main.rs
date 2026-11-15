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
}

impl Check {
    fn new(label: &'static str, tool: Tool, args: &[&str]) -> Self {
        Self {
            label,
            tool,
            args: args.iter().map(OsString::from).collect(),
        }
    }

    fn run(&self, root: &Path) -> Result<(), Failure> {
        eprintln!("\n==> {}", self.label);
        run_command(
            Command::new(self.tool.executable())
                .current_dir(root)
                .args(&self.args),
            self.label,
        )
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
        assert_eq!(full.len(), 11);
        for (basic, full) in basic.iter().zip(&full) {
            assert_eq!(basic.label, full.label);
            assert_eq!(basic.tool, full.tool);
            assert_eq!(basic.args, full.args);
        }
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
