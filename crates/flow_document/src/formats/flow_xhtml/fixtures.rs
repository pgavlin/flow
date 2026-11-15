//! Single inventory shared by syntax, semantic, and rendering fixture tests.

use crate::model::flow::DiagnosticKind;

#[derive(Clone, Copy)]
pub(super) struct Fixture {
    pub name: &'static str,
    pub source: &'static str,
    pub diagnostics: &'static [DiagnosticKind],
}

macro_rules! fixture {
    ($name:ident, $path:literal, [$($diagnostic:ident),*]) => {
        pub(super) const $name: Fixture = Fixture {
            name: $path,
            source: include_str!(concat!("../../../tests/fixtures/flow-semantic-xhtml/", $path)),
            diagnostics: &[$(DiagnosticKind::$diagnostic),*],
        };
    };
}

fixture!(SERIALIZATION, "contract/serialization.xhtml", []);
fixture!(NAMESPACE, "contract/namespace.xhtml", []);
fixture!(BLOCKS, "contract/blocks.xhtml", []);
fixture!(INLINE, "contract/inline.xhtml", []);
fixture!(METADATA, "contract/metadata.xhtml", []);
fixture!(WHITESPACE, "contract/whitespace.xhtml", []);
fixture!(LINKS, "contract/links.xhtml", []);
fixture!(LISTS, "contract/lists.xhtml", []);
fixture!(DEFINITIONS, "contract/definitions.xhtml", []);
fixture!(PREFORMATTED, "contract/preformatted.xhtml", []);
fixture!(IMAGES, "contract/images.xhtml", []);
fixture!(FIGURES, "contract/figures.xhtml", []);
fixture!(TABLES, "contract/tables.xhtml", []);
fixture!(MATH, "contract/math.xhtml", []);
fixture!(DISCLOSURES, "contract/disclosures.xhtml", []);
fixture!(
    MEDIA,
    "contract/media.xhtml",
    [
        UnsupportedMedia,
        UnsupportedMedia,
        UnsupportedMedia,
        DroppedNonTextualMetadata
    ]
);
fixture!(TABLE_SPANS, "regressions/table-spans.xhtml", [InvalidSpan]);
fixture!(
    ORPHAN_TABLE_PARTS,
    "regressions/orphan-table-parts.xhtml",
    [InvalidChild]
);
fixture!(
    ROOT_CODE,
    "regressions/root-code.xhtml",
    [SynthesizedWrapper]
);
fixture!(
    INVALID_METADATA,
    "regressions/invalid-metadata.xhtml",
    [InvalidLanguage, DuplicateAnchor, MissingImageAlternative]
);
fixture!(
    ANONYMOUS_PARAGRAPHS,
    "regressions/anonymous-paragraphs.xhtml",
    []
);
fixture!(GARDEN_SURVEY, "examples/garden-survey.xhtml", []);
fixture!(
    WORKSHOP_MANUAL,
    "examples/workshop-manual.xhtml",
    [UnsupportedMedia]
);
fixture!(
    FIELD_NOTES,
    "examples/field-notes.xhtml",
    [UnsupportedMedia]
);

fixture!(
    IGNORED_ATTRIBUTES,
    "regressions/ignored-attributes.xhtml",
    []
);

pub(super) const CONTRACT: &[Fixture] = &[
    SERIALIZATION,
    NAMESPACE,
    BLOCKS,
    INLINE,
    METADATA,
    WHITESPACE,
    LINKS,
    LISTS,
    DEFINITIONS,
    PREFORMATTED,
    IMAGES,
    FIGURES,
    TABLES,
    MATH,
    DISCLOSURES,
    MEDIA,
];

pub(super) const REGRESSIONS: &[Fixture] = &[
    TABLE_SPANS,
    ORPHAN_TABLE_PARTS,
    ROOT_CODE,
    INVALID_METADATA,
    ANONYMOUS_PARAGRAPHS,
    IGNORED_ATTRIBUTES,
];

pub(super) const EXAMPLES: &[Fixture] = &[GARDEN_SURVEY, WORKSHOP_MANUAL, FIELD_NOTES];

pub(super) fn all() -> impl Iterator<Item = &'static Fixture> {
    CONTRACT.iter().chain(REGRESSIONS).chain(EXAMPLES)
}
