//! Parse a document without a window system or rendering dependencies.
#![allow(clippy::print_stdout, reason = "This example is a host application")]
use flow_document::formats::markdown::{MarkdownOptions, parse};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let imported = parse(
        "# Hello, flow\n\nA **semantic** document with a [link](https://example.com).",
        MarkdownOptions::default(),
    )?;
    println!("{}", imported.document.text().as_str());
    Ok(())
}
