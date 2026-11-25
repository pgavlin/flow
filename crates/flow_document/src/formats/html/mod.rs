//! Internal HTML support shared by source adapters.
//!
//! Syntax frontends own lexical policy (HTML5 recovery versus strict XML).
//! Adapters own source conventions. This module owns neither acquisition nor
//! a public generic HTML importer, and does not use flow XHTML as an IR.

pub(super) mod lowering;
pub(super) mod source;
pub(crate) mod tree;

#[cfg(test)]
mod tests;
