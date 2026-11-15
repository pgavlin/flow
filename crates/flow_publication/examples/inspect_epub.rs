//! Inspect a host-supplied EPUB without engine-owned acquisition.
//! cargo run -p flow_publication --features epub --example inspect_epub -- book.epub
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stdout,
    reason = "This example is a host application"
)]
use flow_publication::{formats::epub::EpubPublication, publication::PublicationKey};
use std::io::Read;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: inspect_epub BOOK.epub")?;
    // This host selects its own acquisition bound, separately from parser limits.
    const HOST_LIMIT: u64 = 128 * 1024 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(HOST_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > HOST_LIMIT {
        return Err("host archive byte limit exceeded".into());
    }
    let source = EpubPublication::from_bytes(bytes)?;
    let model = source.publication(PublicationKey::new("inspection")?)?;
    println!("Titles: {}", model.metadata().titles.join("; "));
    println!("Creators: {}", model.metadata().creators.join("; "));
    println!("Sequential documents: {}", model.reading_order().len());
    println!("Navigation entries: {}", model.navigation().len());
    for diagnostic in source.diagnostics() {
        println!("Diagnostic: {diagnostic:?}");
    }
    Ok(())
}
