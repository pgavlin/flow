//! Allocation measurements, deliberately separate from elapsed-time benchmarks.
mod support;

use std::hint::black_box;

use serde::Serialize;
use support::*;

// This allocator is linked only into this benchmark executable, never the
// library, timing benchmark, or consumers of flow_publication.
#[global_allocator]
static ALLOCATOR: dhat::Alloc = dhat::Alloc;

#[derive(Serialize)]
struct Sample {
    benchmark: String,
    allocated_blocks: u64,
    allocated_bytes: u64,
    peak_live_bytes: usize,
    live_bytes_at_completion: usize,
    live_bytes_after_output_drop: usize,
}

#[allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "Benchmark host owns the allocation profiler lifecycle"
)]
fn measure<T>(benchmark: &str, operation: impl FnOnce() -> T) -> Sample {
    // Testing mode suppresses profile-file creation and stderr summaries.
    // Short backtraces reduce profiling overhead; these measurements use the
    // aggregate counters, not allocation-site attribution.
    let profiler = dhat::Profiler::builder()
        .testing()
        .trim_backtraces(Some(4))
        .build();
    let output = black_box(operation());
    let completed = dhat::HeapStats::get();
    drop(output);
    let dropped = dhat::HeapStats::get();
    drop(profiler);
    Sample {
        benchmark: benchmark.to_owned(),
        allocated_blocks: completed.total_blocks,
        allocated_bytes: completed.total_bytes,
        peak_live_bytes: completed.max_bytes,
        live_bytes_at_completion: completed.curr_bytes,
        live_bytes_after_output_drop: dropped.curr_bytes,
    }
}

#[allow(
    clippy::print_stderr,
    reason = "Benchmark host reports profiling progress"
)]
fn record<T>(samples: &mut Vec<Sample>, filter: &str, name: &str, operation: impl FnOnce() -> T) {
    if selected(filter, name) {
        eprintln!("Profiling {name}");
        samples.push(measure(name, operation));
    }
}

fn selected(filter: &str, name: &str) -> bool {
    match filter.strip_prefix('=') {
        Some(exact) => name == exact,
        None => name.contains(filter),
    }
}

fn check_measurement_boundaries() {
    let sample = measure("self_check", || black_box(vec![0_u8; 1024]));
    assert_eq!(sample.allocated_blocks, 1);
    assert_eq!(sample.allocated_bytes, 1024);
    assert_eq!(sample.peak_live_bytes, 1024);
    assert_eq!(sample.live_bytes_at_completion, 1024);
    assert_eq!(sample.live_bytes_after_output_drop, 0);
}

#[allow(
    clippy::print_stdout,
    clippy::disallowed_methods,
    reason = "Benchmark host reads its CLI arguments and emits its JSON report"
)]
fn main() {
    let args = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--bench")
        .collect::<Vec<_>>();
    assert!(
        args.len() <= 1,
        "usage: memory [substring | =exact-benchmark-name]"
    );
    let filter = args.first().map_or("", String::as_str);
    check_measurement_boundaries();
    let mut samples = Vec::new();
    let source = source(0, false);
    let document = import(&source);
    let fonts = fonts();
    let width = renderer(&fonts).effective_content_width(VIEWPORT.0);
    record(&mut samples, filter, "chapter/import_markdown", || {
        import(black_box(&source))
    });

    let cold_renderer = renderer(&fonts);
    record(&mut samples, filter, "chapter/compose_cold_fonts", || {
        cold_renderer.compose_document(black_box(&document), width)
    });
    drop(cold_renderer);

    let long_source = long_paragraph_source();
    let long_document = import(&long_source);
    let long_renderer = renderer(&fonts);
    record(
        &mut samples,
        filter,
        "chapter/long_paragraph/compose_cold_fonts",
        || long_renderer.compose_document(black_box(&long_document), width),
    );
    let long_composed = long_renderer.compose_document(&long_document, width);
    record(
        &mut samples,
        filter,
        "chapter/long_paragraph/paginate_composed",
        || {
            long_renderer
                .paginate_composed_document(black_box(&long_composed), VIEWPORT.1)
                .unwrap()
        },
    );
    drop(long_composed);
    drop(long_renderer);

    let warm_renderer = renderer(&fonts);
    let composed = warm_renderer.compose_document(&document, width);
    record(&mut samples, filter, "chapter/compose_warm_fonts", || {
        warm_renderer.compose_document(black_box(&document), width)
    });
    record(&mut samples, filter, "chapter/paginate_composed", || {
        warm_renderer
            .paginate_composed_document(black_box(&composed), VIEWPORT.1)
            .unwrap()
    });
    drop(composed);
    drop(warm_renderer);

    for (name, text) in paragraph_scenarios() {
        let name = format!("paragraph_scratch/{name}");
        if !selected(filter, &name) {
            continue;
        }
        let document = import(&text);
        let renderer = renderer(&fonts);
        drop(renderer.compose_document(&document, width));
        record(&mut samples, filter, &name, || {
            renderer.compose_document(black_box(&document), width)
        });
    }

    for (images, cold_name, cached_name) in [
        (
            false,
            "publication_77_chapters/text/cold_open",
            "publication_77_chapters/text/cached_index_open",
        ),
        (
            true,
            "publication_77_chapters/images/cold_open",
            "publication_77_chapters/images/cached_index_open",
        ),
    ] {
        if !selected(filter, cold_name) && !selected(filter, cached_name) {
            continue;
        }
        let book = Book::new(images);
        let index = book.validated_index(images);
        record(&mut samples, filter, cold_name, || book.open(None));
        record(&mut samples, filter, cached_name, || {
            book.open(Some(&index))
        });
    }

    for (name, config) in font_scenarios() {
        let name = format!("font_environments/{name}");
        if !selected(filter, &name) {
            continue;
        }
        let book = Book::configured(config);
        book.validated_index(false);
        record(&mut samples, filter, &name, || book.open(None));
    }

    assert!(!samples.is_empty(), "no workloads matched {filter:?}");

    // Build and serialize the report after profiling, so the output machinery
    // and the samples vector are not included in the allocation measurements.
    let font_options = font_context_options();
    let report = serde_json::json!({
        "schema_version": 1,
        "publication_font_context": {
            "cache_bytes": font_options.cache_bytes,
            "max_environments": font_options.max_environments,
            "plans_per_face": font_options.plans_per_face,
            "max_faces_per_environment": font_options.max_faces_per_environment,
        },
        "allocator": "dhat::Alloc (System)",
        "samples": samples,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
