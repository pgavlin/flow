//! Shared generated workloads for elapsed-time and allocation measurements.
use std::collections::HashMap;

use flow_document::{
    formats::markdown,
    model::flow::FlowDocument,
    render::{FontResource, Renderer},
    settings::{ReadingMode, RenderSettings, Settings},
    test_fonts,
};
use flow_publication::{
    publication::{
        DocumentDescriptor, DocumentKey, ImportedDocument, Publication, PublicationDescription,
        PublicationKey, PublicationMetadata, PublicationRevision, PublicationTarget,
        ResourceDescriptor, ResourceKey, ResourceReference,
    },
    reading_session::{
        PreparedResource, ReadingSession, RequestPurpose, ResourceKind, SessionEffect,
    },
};

const CHAPTERS: usize = 77;
pub const VIEWPORT: (u32, u32) = (674, 896);
const PARAGRAPH: &str = "The reader crossed the quiet library and opened a book beside the window. \
    Its pages described distant rivers, old cities, and journeys through the mountains. \
    Each paragraph carried the story forward, with enough varied words to exercise \
    shaping, line breaking, and the placement of text on a printed page.\n\n";

pub fn settings() -> Settings {
    let mut settings = Settings {
        reading_mode: ReadingMode::Paginated,
        rendering: RenderSettings {
            font_size: 16.0,
            max_width: 414.0,
            body_fonts: vec!["Charter".into()],
            author_fonts: false,
            ..RenderSettings::default()
        },
    };
    settings.theme.page_margin = 61.0;
    settings.theme.line_spacing = 1.35;
    settings
}

pub fn fonts() -> Vec<FontResource> {
    vec![test_fonts::charter_regular(), test_fonts::charter_bold()]
}

pub fn renderer(fonts: &[FontResource]) -> Renderer {
    let mut renderer = Renderer::new(settings());
    assert!(renderer.set_font_resources(fonts.to_vec()));
    renderer
}

pub fn source(index: usize, images: bool) -> String {
    let mut source = format!("# Chapter {}\n\n", index + 1);
    for paragraph in 0..12 {
        source.push_str(PARAGRAPH);
        if images && paragraph % 4 == 0 {
            source.push_str(&format!("![Figure](figure-{paragraph}.png)\n\n"));
        }
    }
    source
}

pub fn long_paragraph_source() -> String {
    // Exercise item/run vector growth independently of the usual short blocks.
    PARAGRAPH.repeat(48).replace('\n', " ")
}

pub fn paragraph_scenarios() -> [(&'static str, String); 2] {
    let small = "A short paragraph of ordinary text.\n\n".repeat(512);
    [
        ("many_small", small.clone()),
        (
            "large_then_small",
            format!("{}\n\n{small}", long_paragraph_source()),
        ),
    ]
}

pub fn import(source: &str) -> FlowDocument {
    markdown::parse(source, Default::default())
        .unwrap()
        .document
}

#[derive(Clone, Copy)]
pub enum FontLayout {
    Global,
    ScopedShared,
    ScopedAliases,
    Alternating,
    Grouped,
    Rotating,
}

#[derive(Clone, Copy)]
pub struct BookConfig {
    pub chapters: usize,
    pub images: bool,
    pub unique_tokens: bool,
    pub font_layout: FontLayout,
}

impl Default for BookConfig {
    fn default() -> Self {
        Self {
            chapters: CHAPTERS,
            images: false,
            unique_tokens: false,
            font_layout: FontLayout::Global,
        }
    }
}

/// Controls for font environment identity, switching frequency, text-cache
/// pressure, and scaling. Both benchmark executables consume this exact matrix.
pub fn font_scenarios() -> Vec<(&'static str, BookConfig)> {
    vec![
        ("shared/77", BookConfig::default()),
        (
            "shared/154",
            BookConfig {
                chapters: 154,
                ..BookConfig::default()
            },
        ),
        (
            "shared/unique_tokens",
            BookConfig {
                unique_tokens: true,
                ..BookConfig::default()
            },
        ),
        (
            "shared/unique_tokens_154",
            BookConfig {
                chapters: 154,
                unique_tokens: true,
                ..BookConfig::default()
            },
        ),
        (
            "scoped_shared/77",
            BookConfig {
                font_layout: FontLayout::ScopedShared,
                ..BookConfig::default()
            },
        ),
        (
            "scoped_aliases/77",
            BookConfig {
                font_layout: FontLayout::ScopedAliases,
                ..BookConfig::default()
            },
        ),
        (
            "alternating/77",
            BookConfig {
                font_layout: FontLayout::Alternating,
                ..BookConfig::default()
            },
        ),
        (
            "rotating_four/77",
            BookConfig {
                font_layout: FontLayout::Rotating,
                ..BookConfig::default()
            },
        ),
        (
            "grouped/77",
            BookConfig {
                font_layout: FontLayout::Grouped,
                ..BookConfig::default()
            },
        ),
        (
            "alternating/unique_tokens",
            BookConfig {
                font_layout: FontLayout::Alternating,
                unique_tokens: true,
                ..BookConfig::default()
            },
        ),
    ]
}

fn unique_source(index: usize, images: bool) -> String {
    let mut text = format!("# Chapter {}\n\n", index + 1);
    for paragraph in 0..12 {
        // Deliberately unique shaping inputs: a stress case for the shaped-text
        // cache, not a claim to model natural-language vocabulary distribution.
        for word in 0..55 {
            text.push_str(&format!("token{index}x{paragraph}x{word} "));
        }
        text.push_str("\n\n");
        if images && paragraph % 4 == 0 {
            text.push_str(&format!("![Figure](figure-{paragraph}.png)\n\n"));
        }
    }
    text
}

#[allow(
    clippy::disallowed_methods,
    reason = "Benchmark host reads cache-policy overrides outside measurement"
)]
pub fn font_context_options() -> flow_document::font_context::FontContextOptions {
    let mut options = flow_document::font_context::FontContextOptions::default();
    if let Ok(bytes) = std::env::var("FLOW_BENCH_FONT_CACHE_BYTES") {
        options.cache_bytes = bytes
            .parse()
            .expect("FLOW_BENCH_FONT_CACHE_BYTES must be an unsigned integer");
    }
    if let Ok(capacity) = std::env::var("FLOW_BENCH_FONT_ENVIRONMENTS") {
        options.max_environments = capacity
            .parse()
            .expect("FLOW_BENCH_FONT_ENVIRONMENTS must be an unsigned integer");
    }
    if std::env::var_os("FLOW_BENCH_DISABLE_PLAN_CACHE").is_some() {
        options.plans_per_face = 0;
    }
    options
}

pub struct Book {
    font_options: flow_document::font_context::FontContextOptions,
    publication: Publication,
    sources: HashMap<DocumentKey, String>,
    fonts: Vec<FontResource>,
    scoped_fonts: HashMap<ResourceKey, FontResource>,
    config: BookConfig,
}

#[derive(Default)]
pub struct Work {
    pub imports: usize,
    pub pagination_imports: usize,
    pub prefetch_imports: usize,
    pub metadata_replies: usize,
    pub font_replies: usize,
}

impl Book {
    pub fn validated_index(&self, images: bool) -> Vec<Option<u32>> {
        let (session, work) = self.open(None);
        assert_eq!(work.imports, self.config.chapters);
        assert_eq!(work.prefetch_imports, 1);
        assert_eq!(images, self.config.images);
        assert_eq!(
            work.metadata_replies,
            if images { self.config.chapters * 3 } else { 0 }
        );
        let expected_fonts = match self.config.font_layout {
            FontLayout::Global => 0,
            FontLayout::ScopedAliases => self.config.chapters * 2,
            _ => 2,
        };
        assert_eq!(work.font_replies, expected_fonts);
        let index = session.publication_page_index().unwrap().to_vec();
        let (_, warm_work) = self.open(Some(&index));
        assert_eq!(warm_work.pagination_imports, 0);
        assert_eq!(warm_work.imports, 2);
        index
    }

    pub fn new(images: bool) -> Self {
        Self::configured(BookConfig {
            images,
            ..BookConfig::default()
        })
    }

    pub fn configured(config: BookConfig) -> Self {
        assert!(config.chapters >= 2);
        let font_data = fonts();
        let mut scoped_fonts = HashMap::new();
        let documents = (0..config.chapters)
            .map(|index| DocumentDescriptor {
                key: DocumentKey::new(format!("chapter-{index}")).unwrap(),
                title: format!("Chapter {}", index + 1),
                fonts: match config.font_layout {
                    FontLayout::Global => Vec::new(),
                    layout => {
                        let count = match layout {
                            FontLayout::Rotating if index % 4 < 2 => 1,
                            FontLayout::Alternating if index % 2 == 0 => 1,
                            FontLayout::Grouped if index < config.chapters.div_ceil(2) => 1,
                            _ => 2,
                        };
                        (0..count)
                            .map(|font| {
                                let font = if matches!(layout, FontLayout::Rotating)
                                    && matches!(index % 4, 1 | 3)
                                {
                                    1 - font
                                } else {
                                    font
                                };
                                let name = if matches!(layout, FontLayout::ScopedAliases) {
                                    format!("chapter-{index}-font-{font}")
                                } else {
                                    format!("font-{font}")
                                };
                                let key = ResourceKey::new(name).unwrap();
                                scoped_fonts.insert(key.clone(), font_data[font].clone());
                                key
                            })
                            .collect()
                    }
                },
            })
            .collect::<Vec<_>>();
        let sources = documents
            .iter()
            .enumerate()
            .map(|(index, document)| {
                let source = if config.unique_tokens {
                    unique_source(index, config.images)
                } else {
                    source(index, config.images)
                };
                (document.key.clone(), source)
            })
            .collect();
        let publication = Publication::new(PublicationDescription {
            key: PublicationKey::new("synthetic-benchmark").unwrap(),
            revision: PublicationRevision::new("1").unwrap(),
            metadata: PublicationMetadata::default(),
            reading_order: documents
                .iter()
                .map(|document| document.key.clone())
                .collect(),
            documents,
            navigation: Vec::new(),
            resources: {
                // Keep descriptor order deterministic even though host lookup
                // is a map. Registration order within each document is fixed.
                let mut keys = scoped_fonts.keys().cloned().collect::<Vec<_>>();
                keys.sort();
                keys.into_iter()
                    .map(|key| ResourceDescriptor {
                        key,
                        media_type: Some("font/ttf".into()),
                    })
                    .collect()
            },
            start: None,
        })
        .unwrap();
        Self {
            font_options: font_context_options(),
            publication,
            sources,
            fonts: if matches!(config.font_layout, FontLayout::Global) {
                font_data
            } else {
                Vec::new()
            },
            scoped_fonts,
            config,
        }
    }

    /// Fresh engine/session caches every time, but all source/font bytes are
    /// already in memory. There is deliberately no filesystem or EPUB adapter.
    pub fn open(&self, index: Option<&[Option<u32>]>) -> (ReadingSession, Work) {
        let mut session = ReadingSession::new_with_font_context(
            settings(),
            flow_document::font_context::FontContext::new(self.font_options),
        );
        session.resize(VIEWPORT);
        for font in &self.fonts {
            assert!(session.register_font_resource(font.clone()));
        }
        let handle = session
            .register_publication(self.publication.clone())
            .unwrap();
        session
            .open_target(
                handle,
                PublicationTarget::document(self.publication.reading_order()[0].clone()),
            )
            .unwrap();
        let mut work = Work::default();
        self.service(&mut session, &mut work);
        if let Some(index) = index {
            assert!(session.install_publication_page_index(index.to_vec()));
        }
        assert!(session.prepare_publication_pagination().unwrap());
        self.service(&mut session, &mut work);
        assert!(session.publication_pagination_complete());
        assert_eq!(
            session.publication_page_index().unwrap().len(),
            self.config.chapters
        );
        (session, work)
    }

    /// Respond in effect order, including one metadata reply at a time, rather
    /// than batching all image dimensions before the engine can advance.
    fn service(&self, session: &mut ReadingSession, work: &mut Work) {
        for _ in 0..self.config.chapters * 20 {
            let effects = session.take_effects().unwrap();
            let mut more = false;
            for effect in effects {
                match effect {
                    SessionEffect::LoadDocument(request) => {
                        work.imports += 1;
                        work.pagination_imports +=
                            usize::from(request.purpose == RequestPurpose::Pagination);
                        work.prefetch_imports +=
                            usize::from(request.purpose == RequestPurpose::Prefetch);
                        let document = import(&self.sources[&request.document]);
                        session
                            .provide_document(request.id, ImportedDocument::new(document))
                            .unwrap();
                        more = true;
                    }
                    SessionEffect::LoadResource(request) if request.kind == ResourceKind::Font => {
                        let ResourceReference::Publication(resource) = request.resource else {
                            panic!("font request without a publication resource");
                        };
                        work.font_replies += 1;
                        session
                            .provide_resource(
                                request.id,
                                PreparedResource::Font(self.scoped_fonts[&resource].clone()),
                            )
                            .unwrap();
                        more = true;
                    }
                    SessionEffect::LoadResource(request) => {
                        assert_eq!(request.kind, ResourceKind::ImageMetadata);
                        work.metadata_replies += 1;
                        session
                            .provide_resource(
                                request.id,
                                PreparedResource::ImageMetadata {
                                    width: 640,
                                    height: 480,
                                },
                            )
                            .unwrap();
                        more = true;
                    }
                    SessionEffect::ContinueWork => more = true,
                    SessionEffect::ResolvePublication { .. }
                    | SessionEffect::ResolveLink { .. } => {
                        panic!("unexpected host resolution: {effect:?}");
                    }
                    _ => (),
                }
            }
            if !more {
                return;
            }
        }
        panic!("synthetic publication produced unbounded work");
    }
}
