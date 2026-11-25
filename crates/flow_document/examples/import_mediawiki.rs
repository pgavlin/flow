//! cargo run -p flow_document --features mediawiki-html --example import_mediawiki -- article.html https://example.org/wiki/Article
#![allow(
    clippy::disallowed_methods,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "This example is a host application"
)]
use flow_document::formats::mediawiki_html::{MediaWikiOptions, import};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: import_mediawiki ARTICLE.html [SOURCE_URL]")?;
    let source = std::fs::read_to_string(path)?;
    let outcome = import(
        &source,
        MediaWikiOptions {
            source_url: args.next(),
            ..Default::default()
        },
    )?;
    eprintln!("metadata: {:#?}", outcome.metadata);
    for diagnostic in outcome.content.diagnostics {
        eprintln!("{diagnostic:?}");
    }
    println!("{}", outcome.content.document.text().as_str());
    Ok(())
}
