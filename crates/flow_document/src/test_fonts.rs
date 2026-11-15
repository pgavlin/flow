//! Bundled font resources for testing hosts (`test-fonts`).
//! Fixtures and their licenses ship with this crate.

use crate::text::FontResource;
use std::sync::Arc;

pub fn charter_regular() -> FontResource {
    FontResource::from_bytes(Arc::from(
        include_bytes!("../tests/fixtures/fonts/charter.ttf").as_slice(),
    ))
}

pub fn charter_bold() -> FontResource {
    FontResource::from_bytes(Arc::from(
        include_bytes!("../tests/fixtures/fonts/charter-bold.ttf").as_slice(),
    ))
}
