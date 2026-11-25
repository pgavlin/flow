use super::super::html::lowering::LoweringPolicy;
use super::*;
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct Policy {
    pub relations: HashMap<String, LinkRelation>,
    pub notes: HashSet<String>,
    pub math: HashMap<String, (String, MathDisplay)>,
    pub images: HashMap<String, responsive::PendingImage>,
}

fn key(element: &Element) -> &str {
    element
        .attribute(None, "data-flow-import-key")
        .unwrap_or_default()
}

impl LoweringPolicy for Policy {
    fn preformatted_all_children(&self) -> bool {
        true
    }
    fn media_flow_fallback(&self) -> bool {
        true
    }
    fn preserve_figure_flow(&self) -> bool {
        true
    }
    fn atomic_spacing_metadata(&self) -> bool {
        false
    }
    fn zero_row_span_to_end(&self) -> bool {
        true
    }
    fn flow_container(&self, element: &Element) -> Option<NodeKind> {
        (element.name().local() == "section").then_some(NodeKind::Section)
    }
    fn inline_semantic(&self, element: &Element) -> Option<InlineSemantic> {
        let role = match element.name().local() {
            "del" => InlineRole::Deletion,
            "ins" => InlineRole::Insertion,
            "kbd" => InlineRole::KeyboardInput,
            "samp" => InlineRole::SampleOutput,
            "var" => InlineRole::Variable,
            "dfn" => InlineRole::Definition,
            "small" => InlineRole::Small,
            "abbr" => InlineRole::Abbreviation,
            "time" => InlineRole::Time,
            _ => return None,
        };
        let value = match role {
            InlineRole::Abbreviation => element.attribute(None, "title"),
            InlineRole::Time => element.attribute(None, "datetime"),
            _ => None,
        }
        .map(str::to_owned);
        Some(InlineSemantic { role, value })
    }
    fn ruby(&self, element: &Element) -> bool {
        element.name().local() == "ruby"
    }
    fn inline_atom(&self, element: &Element) -> Option<NodeKind> {
        (element.name().local() == "wbr").then_some(NodeKind::BreakOpportunity(BreakOpportunity {
            hyphen_when_broken: false,
        }))
    }
    fn link_content(&self, _element: &Element, flow: bool) -> LinkContent {
        if flow {
            LinkContent::Flow
        } else {
            LinkContent::Phrasing
        }
    }
    fn link_relation(&self, element: &Element) -> LinkRelation {
        self.relations
            .get(key(element))
            .cloned()
            .unwrap_or(LinkRelation::Ordinary)
    }
    fn roles(&self, element: &Element) -> Vec<SemanticRole> {
        if self.notes.contains(key(element)) {
            vec![SemanticRole::Known(KnownRole::Footnote)]
        } else {
            Vec::new()
        }
    }
    fn title(&self, element: &Element) -> Option<String> {
        element
            .attribute(None, "title")
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }
    fn math_metadata(&self, element: &Element) -> Option<(String, MathDisplay)> {
        self.math.get(key(element)).cloned()
    }
    fn cell_scope(&self, element: &Element) -> Option<HeaderScope> {
        match element
            .attribute(None, "scope")?
            .to_ascii_lowercase()
            .as_str()
        {
            "row" => Some(HeaderScope::Row),
            "col" => Some(HeaderScope::Column),
            "rowgroup" => Some(HeaderScope::RowGroup),
            "colgroup" => Some(HeaderScope::ColumnGroup),
            _ => None,
        }
    }
    fn cell_headers(&self, element: &Element) -> Option<Vec<String>> {
        element
            .attribute(None, "headers")
            .map(|value| value.split_ascii_whitespace().map(str::to_owned).collect())
    }
    fn bidi(&self, element: &Element) -> Option<BidiState> {
        let direction = match element.attribute(None, "dir") {
            Some("ltr") => BaseDirection::LeftToRight,
            Some("rtl") => BaseDirection::RightToLeft,
            _ => BaseDirection::Auto,
        };
        match element.name().local() {
            "bdi" => Some(BidiState {
                direction,
                mode: BidiMode::Isolate,
            }),
            "bdo" if direction != BaseDirection::Auto => Some(BidiState {
                direction,
                mode: BidiMode::Override,
            }),
            _ => None,
        }
    }
    fn responsive_image(
        &self,
        element: &Element,
        builder: &mut FlowBuilder,
    ) -> Result<Option<ResponsiveImage>, BuildError> {
        self.images
            .get(key(element))
            .map(|image| image.lower(builder))
            .transpose()
    }
}
