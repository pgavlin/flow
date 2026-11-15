//! Browser-demo assembly, kept out of engine build scripts and dependencies.
use super::{Check, Failure, Tool};
use std::{fs, path::Path, process::Command};

const BINDGEN_VERSION: &str = "0.2.129";
const INSTALL: &str = "cargo install --locked wasm-bindgen-cli --version 0.2.129";
const OUTPUT: &str = "examples/reader/target/web";

fn verify_version(version: &str) -> Result<(), Failure> {
    if version.trim() == format!("wasm-bindgen {BINDGEN_VERSION}") {
        return Ok(());
    }
    Err(Failure {
        code: 1,
        message: format!(
            "expected wasm-bindgen {BINDGEN_VERSION}, found {version:?}\nInstall: {INSTALL}"
        ),
    })
}

fn plan(dev: bool) -> [Check; 2] {
    let profile = if dev { "dev" } else { "release" };
    let artifact = if dev { "debug" } else { "release" };
    [
        Check::new(
            "Build browser reader",
            Tool::Cargo,
            &[
                "build",
                "--locked",
                "--manifest-path",
                "examples/reader/Cargo.toml",
                "--lib",
                "--target",
                "wasm32-unknown-unknown",
                "--profile",
                profile,
                // A dedicated target directory makes output independent of the
                // caller's CARGO_TARGET_DIR and keeps the documented serve path fixed.
                "--target-dir",
                "examples/reader/target",
            ],
        ),
        Check::new(
            "Generate browser bindings",
            Tool::WasmBindgen,
            &[
                "--target",
                "web",
                "--out-dir",
                "examples/reader/target/web/pkg",
                &format!(
                    "examples/reader/target/wasm32-unknown-unknown/{artifact}/flow_demo_reader.wasm"
                ),
            ],
        ),
    ]
}

pub(super) fn build(root: &Path, dev: bool) -> Result<(), Failure> {
    let version = Command::new(Tool::WasmBindgen.executable())
        .arg("--version")
        .output()
        .map_err(|error| Failure {
            code: 1,
            message: format!("could not run wasm-bindgen: {error}\nInstall: {INSTALL}"),
        })?;
    if !version.status.success() {
        return Err(Failure {
            code: 1,
            message: format!(
                "wasm-bindgen --version failed: {}",
                String::from_utf8_lossy(&version.stderr)
            ),
        });
    }
    verify_version(&String::from_utf8_lossy(&version.stdout))?;
    let [compile, bindgen] = plan(dev);
    compile.run(root)?;
    fs::create_dir_all(root.join(OUTPUT).join("pkg")).map_err(|error| Failure {
        code: 1,
        message: format!("could not create web output directory: {error}"),
    })?;
    bindgen.run(root)?;
    fs::copy(
        root.join("examples/reader/index.html"),
        root.join(OUTPUT).join("index.html"),
    )
    .map_err(|error| Failure {
        code: 1,
        message: format!("could not copy reader HTML: {error}"),
    })?;
    eprintln!("\nServe with: python3 -m http.server 8080 --directory {OUTPUT}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_version_matches_the_pinned_reader_dependency() {
        assert!(
            include_str!("../../../examples/reader/Cargo.toml")
                .contains(&format!("wasm-bindgen = \"={BINDGEN_VERSION}\""))
        );
        verify_version(&format!("wasm-bindgen {BINDGEN_VERSION}\n")).unwrap();
        assert!(INSTALL.ends_with(BINDGEN_VERSION));
        assert_eq!(verify_version("wasm-bindgen 0.0.0").unwrap_err().code, 1);
        assert!(verify_version("unexpected output").is_err());
    }
    #[test]
    fn profiles_and_artifacts_match_without_shell_commands() {
        for (dev, profile, artifact) in [(false, "release", "release"), (true, "dev", "debug")] {
            let [compile, bindgen] = plan(dev);
            assert_eq!(compile.tool, Tool::Cargo);
            assert!(
                compile
                    .args
                    .windows(2)
                    .any(|pair| pair == ["--profile", profile])
            );
            assert_eq!(bindgen.tool, Tool::WasmBindgen);
            assert_eq!(
                bindgen.args.last().unwrap(),
                &format!(
                    "examples/reader/target/wasm32-unknown-unknown/{artifact}/flow_demo_reader.wasm"
                )
                .as_str()
            );
            assert!(
                compile
                    .args
                    .windows(2)
                    .any(|pair| pair == ["--target-dir", "examples/reader/target"])
            );
        }
    }
}
