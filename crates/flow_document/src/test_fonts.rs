//! Repository font fixtures for testing hosts (`test-fonts`).
//! Fonts are not shipped in crate archives. Set `FLOW_TEST_FONT_DIR` to a
//! directory containing the repository's font fixtures when testing elsewhere.

use crate::text::FontResource;
use std::sync::Arc;

/// Read a repository fixture, or panic with its path if it is unavailable.
#[allow(clippy::disallowed_methods, reason = "Explicit test-host fixture I/O")]
pub(crate) fn fixture(name: &str) -> Vec<u8> {
    let directory = std::env::var_os("FLOW_TEST_FONT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fonts")
        });
    let path = directory.join(name);
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!("cannot read test font {}: {error}; set FLOW_TEST_FONT_DIR to the repository font directory", path.display())
    })
}

pub fn charter_regular() -> FontResource {
    FontResource::from_bytes(Arc::from(fixture("charter.ttf")))
}

pub fn charter_bold() -> FontResource {
    FontResource::from_bytes(Arc::from(fixture("charter-bold.ttf")))
}
