//! Exact pre-extraction fingerprints of public importer outcomes, including all
//! document catalogs, IDs, diagnostics, errors, and retained source bytes.
//! Update intentionally with UPDATE_HTML_IMPORT_BASELINE=1 cargo test -p
//! flow_document --test html_import_baseline (never during an extraction).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use flow_document::formats::{epub_xhtml, flow_xhtml};
use flow_document::model::flow::{DocumentReference, SourceRetention};

#[allow(
    clippy::disallowed_methods,
    reason = "Test host reads checked-in fixtures and explicitly updates baselines"
)]
fn fixtures(root: &Path, paths: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            fixtures(&path, paths);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "xhtml")
        {
            paths.push(path);
        }
    }
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "Test host reads fixtures; baseline writes require explicit opt-in"
)]
fn exact_import_outcomes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture_root = root.join("tests/fixtures/flow-semantic-xhtml");
    let mut paths = Vec::new();
    fixtures(&fixture_root, &mut paths);
    paths.sort();
    let mut actual = String::new();
    let mut record = |name: &str, outcome: &dyn std::fmt::Debug| {
        let digest = blake3::hash(format!("{outcome:#?}").as_bytes());
        writeln!(actual, "{name}: {digest}").unwrap();
    };
    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap();
        let name = path
            .strip_prefix(&fixture_root)
            .unwrap()
            .components()
            .map(|component| component.as_os_str().to_str().unwrap())
            .collect::<Vec<_>>()
            .join("/");
        for version in [0, 1] {
            for retain in [false, true] {
                let result = flow_xhtml::import(
                    &source,
                    flow_xhtml::XhtmlOptions {
                        canonical_version: version,
                        resource_base: Some("https://example.test/book/chapter.xhtml".into()),
                        identity: Some(DocumentReference::Host("baseline".into())),
                        retention: if retain {
                            SourceRetention::Retain
                        } else {
                            SourceRetention::Discard
                        },
                    },
                );
                record(&format!("flow/{name}/v{version}/retain={retain}"), &result);
            }
        }
        // Exercise the same content through the complete-document/CSS adapter.
        let epub = format!(
            "<!DOCTYPE html><html xmlns='http://www.w3.org/1999/xhtml'><head><title>Not body text</title></head><body>{source}</body></html>"
        );
        for styled in [false, true] {
            let result = epub_xhtml::import(
                &epub,
                epub_xhtml::EpubXhtmlOptions {
                    identity: Some(DocumentReference::PublicationItem("baseline".into())),
                    resource_base: Some("epub://baseline/chapter.xhtml".into()),
                    retention: SourceRetention::Retain,
                    stylesheets: if styled {
                        vec!["body { font-family: serif; } p { margin: 1em; text-indent: 2em; } h2 { font-size: 2em; } span { font-weight: bold; } td { padding: 3px; }".into()]
                    } else {
                        Vec::new()
                    },
                },
            );
            record(&format!("epub/{name}/styled={styled}"), &result);
        }
    }
    for (index, source) in [
        "",
        "<p>",
        "<p>&nbsp;</p>",
        "<!DOCTYPE p><p/>",
        "<p a='1' a='2'/>",
        "<p xmlns:x='urn:x' x:a='1'/>",
        "<p><foreign:span/></p>",
        "<p id='x'>a<span id='x' lang='invalid!'>b</span></p>",
        "<svg xmlns='http://www.w3.org/2000/svg'><p>foreign</p></svg>",
        "<html xmlns='http://www.w3.org/1999/xhtml'><head/></html>",
        "<!DOCTYPE p [<!ENTITY x 'x'>]><p>&x;</p>",
    ]
    .iter()
    .enumerate()
    {
        record(
            &format!("flow/error-boundary/{index}"),
            &flow_xhtml::import(source, Default::default()),
        );
        record(
            &format!("epub/error-boundary/{index}"),
            &epub_xhtml::import(source, Default::default()),
        );
    }
    record(
        "flow/unsupported-version",
        &flow_xhtml::import(
            "<p/>",
            flow_xhtml::XhtmlOptions {
                canonical_version: 2,
                ..Default::default()
            },
        ),
    );
    let baseline = root.join("tests/fixtures/html-import-baseline.txt");
    if std::env::var_os("UPDATE_HTML_IMPORT_BASELINE").is_some() {
        std::fs::write(baseline, &actual).unwrap();
    } else {
        assert_eq!(
            actual,
            std::fs::read_to_string(baseline).unwrap(),
            "Public importer behavior changed"
        );
    }
}
