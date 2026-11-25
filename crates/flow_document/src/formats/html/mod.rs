//! Internal HTML support shared by source adapters.
//!
//! Syntax frontends own lexical policy (HTML5 recovery versus strict XML).
//! Adapters own source conventions. This module owns neither acquisition nor
//! a public generic HTML importer, and does not use flow XHTML as an IR.

pub(super) mod lowering;
#[cfg(feature = "html-parser")]
// The internal parser can be built/tested without an enabled source adapter.
#[cfg_attr(
    not(any(feature = "web-html", feature = "mediawiki-html")),
    allow(dead_code)
)]
pub(super) mod parser;
pub(super) mod source;
pub(crate) mod tree;

#[cfg(test)]
mod tests;
