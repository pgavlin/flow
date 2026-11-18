//! Synthetic engine and publication-opening timing benchmarks.
mod support;

use std::{hint::black_box, time::Duration};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use support::*;

fn chapter_benchmarks(c: &mut Criterion) {
    let source = source(0, false);
    let document = import(&source);
    let fonts = fonts();
    let mut group = c.benchmark_group("chapter");
    group.throughput(Throughput::Bytes(source.len() as u64));
    group.bench_function("import_markdown", |b| b.iter(|| import(black_box(&source))));
    let width = renderer(&fonts).effective_content_width(VIEWPORT.0);
    group.bench_function("compose_cold_fonts", |b| {
        b.iter_batched_ref(
            || renderer(&fonts),
            |renderer| renderer.compose_document(black_box(&document), width),
            BatchSize::LargeInput,
        );
    });
    let warm_renderer = renderer(&fonts);
    let composed = warm_renderer.compose_document(&document, width);
    group.bench_function("compose_warm_fonts", |b| {
        b.iter(|| warm_renderer.compose_document(black_box(&document), width));
    });
    group.bench_function("paginate_composed", |b| {
        b.iter(|| {
            warm_renderer
                .paginate_composed_document(black_box(&composed), VIEWPORT.1)
                .unwrap()
        });
    });
    let long_source = long_paragraph_source();
    let long_document = import(&long_source);
    group.throughput(Throughput::Bytes(long_source.len() as u64));
    group.bench_function("long_paragraph/compose_cold_fonts", |b| {
        b.iter_batched_ref(
            || renderer(&fonts),
            |renderer| renderer.compose_document(black_box(&long_document), width),
            BatchSize::LargeInput,
        );
    });
    let long_composed = warm_renderer.compose_document(&long_document, width);
    group.bench_function("long_paragraph/paginate_composed", |b| {
        b.iter(|| {
            warm_renderer
                .paginate_composed_document(black_box(&long_composed), VIEWPORT.1)
                .unwrap()
        });
    });
    group.finish();
}

fn publication_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("publication_77_chapters");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));
    // One operation is a complete open, not 77 chapter imports: cached-index
    // opens intentionally load only the active chapter and its neighbor.
    group.throughput(Throughput::Elements(1));
    for (name, images) in [("text", false), ("images", true)] {
        let book = Book::new(images);
        let index = book.validated_index(images);
        group.bench_function(format!("{name}/cold_open"), |b| {
            b.iter(|| black_box(book.open(None)));
        });
        group.bench_function(format!("{name}/cached_index_open"), |b| {
            b.iter(|| black_box(book.open(Some(&index))));
        });
    }
    group.finish();
}

fn font_environment_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("font_environments");
    group.sample_size(10);
    group.throughput(Throughput::Elements(1));
    for (name, config) in font_scenarios() {
        let book = Book::configured(config);
        book.validated_index(false);
        group.bench_function(name, |b| b.iter(|| black_box(book.open(None))));
    }
    group.finish();
}

fn paragraph_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("paragraph_scratch");
    let fonts = fonts();
    for (name, text) in paragraph_scenarios() {
        let document = import(&text);
        let renderer = renderer(&fonts);
        let width = renderer.effective_content_width(VIEWPORT.0);
        drop(renderer.compose_document(&document, width));
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_function(name, |b| {
            b.iter(|| renderer.compose_document(black_box(&document), width));
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    chapter_benchmarks,
    publication_benchmarks,
    font_environment_benchmarks,
    paragraph_benchmarks
);
criterion_main!(benches);
