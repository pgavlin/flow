use std::fmt;
use std::hash::Hash;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser, ToCss,
};
use precomputed_hash::PrecomputedHash;
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::context::{
    MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode, SelectorCaches,
};
use selectors::matching::{ElementSelectorFlags, MatchingContext, matches_selector};
use selectors::parser::{
    NonTSPseudoClass, ParseRelative, Parser as SelectorParser, PseudoElement, SelectorImpl,
    SelectorList, SelectorParseErrorKind,
};
use selectors::{Element, OpaqueElement};

use crate::formats::xhtml::{SourceNodeId, XhtmlSourceNodeKind, XhtmlSourceTree};
use crate::model::flow::{
    AuthorBreak, AuthorFontFamily, AuthorStyle, CssBorder, CssColor, CssLength, TextAlignment,
};

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
struct Atom(String);

impl From<&str> for Atom {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl ToCss for Atom {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_identifier(&self.0, dest)
    }
}

impl PrecomputedHash for Atom {
    fn precomputed_hash(&self) -> u32 {
        let mut hash = 2_166_136_261_u32;
        for byte in self.0.as_bytes() {
            hash ^= u32::from(*byte);
            hash = hash.wrapping_mul(16_777_619);
        }
        hash
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AttrValue(String);

impl From<&str> for AttrValue {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl AsRef<str> for AttrValue {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl ToCss for AttrValue {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_string(&self.0, dest)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UnsupportedPseudo;

impl ToCss for UnsupportedPseudo {
    fn to_css<W: fmt::Write>(&self, _dest: &mut W) -> fmt::Result {
        Ok(())
    }
}

impl NonTSPseudoClass for UnsupportedPseudo {
    fn is_active_or_hover(&self) -> bool {
        false
    }

    fn is_user_action_state(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum EpubPseudoElement {
    FirstLetter,
}

impl ToCss for EpubPseudoElement {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str("::first-letter")
    }
}

impl PseudoElement for EpubPseudoElement {}

#[derive(Clone, Debug)]
struct EpubSelectorImpl;

impl SelectorImpl for EpubSelectorImpl {
    type ExtraMatchingData<'a> = std::marker::PhantomData<&'a ()>;
    type AttrValue = AttrValue;
    type Identifier = Atom;
    type LocalName = Atom;
    type NamespaceUrl = Atom;
    type NamespacePrefix = Atom;
    type BorrowedLocalName = Atom;
    type BorrowedNamespaceUrl = Atom;
    type NonTSPseudoClass = UnsupportedPseudo;
    type PseudoElement = EpubPseudoElement;
}

#[derive(Default)]
struct EpubSelectorParser;

impl<'i> SelectorParser<'i> for EpubSelectorParser {
    type Impl = EpubSelectorImpl;
    type Error = SelectorParseErrorKind;

    fn parse_pseudo_element(
        &self,
        name: &str,
    ) -> Result<EpubPseudoElement, ParseError<Self::Error>> {
        if name.eq_ignore_ascii_case("first-letter") {
            Ok(EpubPseudoElement::FirstLetter)
        } else {
            Err(ParseError::custom(
                SelectorParseErrorKind::UnsupportedPseudoClassOrElement,
            ))
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TreeElement<'a> {
    tree: &'a XhtmlSourceTree,
    id: SourceNodeId,
    pseudo: bool,
}

impl TreeElement<'_> {
    fn element(&self) -> &super::XhtmlElement {
        let XhtmlSourceNodeKind::Element(element) = self.tree.get(self.id).unwrap().kind() else {
            unreachable!("selector adapters only wrap elements")
        };
        element
    }

    fn sibling(&self, previous: bool) -> Option<Self> {
        let parent = self.tree.get(self.id)?.parent()?;
        let children = self.tree.get(parent)?.children();
        let index = children.iter().position(|child| *child == self.id)?;
        let candidates: Box<dyn Iterator<Item = &SourceNodeId>> = if previous {
            Box::new(children[..index].iter().rev())
        } else {
            Box::new(children[index + 1..].iter())
        };
        candidates
            .copied()
            .find(|child| {
                matches!(
                    self.tree.get(*child).map(|node| node.kind()),
                    Some(XhtmlSourceNodeKind::Element(_))
                )
            })
            .map(|id| Self {
                tree: self.tree,
                id,
                pseudo: false,
            })
    }
}

impl Element for TreeElement<'_> {
    type Impl = EpubSelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.tree.get(self.id).unwrap())
    }

    fn parent_element(&self) -> Option<Self> {
        let mut parent = self.tree.get(self.id)?.parent();
        while let Some(id) = parent {
            let node = self.tree.get(id)?;
            if matches!(node.kind(), XhtmlSourceNodeKind::Element(_)) {
                return Some(Self {
                    tree: self.tree,
                    id,
                    pseudo: false,
                });
            }
            parent = node.parent();
        }
        None
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn pseudo_element_originating_element(&self) -> Option<Self> {
        self.pseudo.then_some(Self {
            tree: self.tree,
            id: self.id,
            pseudo: false,
        })
    }

    fn is_pseudo_element(&self) -> bool {
        self.pseudo
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.sibling(true)
    }

    fn next_sibling_element(&self) -> Option<Self> {
        self.sibling(false)
    }

    fn first_element_child(&self) -> Option<Self> {
        self.tree
            .get(self.id)?
            .children()
            .iter()
            .copied()
            .find(|child| {
                matches!(
                    self.tree.get(*child).map(|node| node.kind()),
                    Some(XhtmlSourceNodeKind::Element(_))
                )
            })
            .map(|id| Self {
                tree: self.tree,
                id,
                pseudo: false,
            })
    }

    fn is_html_element_in_html_document(&self) -> bool {
        false
    }

    fn has_local_name(&self, name: &Atom) -> bool {
        self.element().name().local() == name.0
    }

    fn has_namespace(&self, namespace: &Atom) -> bool {
        self.element().name().namespace().unwrap_or_default() == namespace.0
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.element().name().local() == other.element().name().local()
            && self.element().name().namespace() == other.element().name().namespace()
    }

    fn attr_matches(
        &self,
        namespace: &NamespaceConstraint<&Atom>,
        local_name: &Atom,
        operation: &AttrSelectorOperation<&AttrValue>,
    ) -> bool {
        self.element().attributes().iter().any(|attribute| {
            let namespace_matches = match namespace {
                NamespaceConstraint::Any => true,
                NamespaceConstraint::Specific(expected) => {
                    attribute.name().namespace().unwrap_or_default() == expected.0
                }
            };
            namespace_matches
                && attribute.name().local() == local_name.0
                && operation.eval_str(attribute.value())
        })
    }

    fn match_non_ts_pseudo_class(
        &self,
        _pseudo: &UnsupportedPseudo,
        _context: &mut MatchingContext<EpubSelectorImpl>,
    ) -> bool {
        false
    }

    fn match_pseudo_element(
        &self,
        pseudo: &EpubPseudoElement,
        _context: &mut MatchingContext<EpubSelectorImpl>,
    ) -> bool {
        matches!(pseudo, EpubPseudoElement::FirstLetter)
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        self.element().name().local() == "a" && self.element().attribute(None, "href").is_some()
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &Atom, sensitivity: CaseSensitivity) -> bool {
        self.element()
            .attribute(None, "id")
            .is_some_and(|value| sensitivity.eq(value.as_bytes(), id.0.as_bytes()))
    }

    fn has_class(&self, class: &Atom, sensitivity: CaseSensitivity) -> bool {
        self.element()
            .attribute(None, "class")
            .is_some_and(|value| {
                value
                    .split_ascii_whitespace()
                    .any(|candidate| sensitivity.eq(candidate.as_bytes(), class.0.as_bytes()))
            })
    }

    fn has_custom_state(&self, _name: &Atom) -> bool {
        false
    }

    fn imported_part(&self, _name: &Atom) -> Option<Atom> {
        None
    }

    fn is_part(&self, _name: &Atom) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        !self
            .tree
            .get(self.id)
            .unwrap()
            .children()
            .iter()
            .any(
                |child| match self.tree.get(*child).map(|node| node.kind()) {
                    Some(XhtmlSourceNodeKind::Element(_)) => true,
                    Some(XhtmlSourceNodeKind::Text(value)) => !value.is_empty(),
                    _ => false,
                },
            )
    }

    fn is_root(&self) -> bool {
        self.parent_element().is_none()
    }

    fn add_element_unique_hashes(&self, _filter: &mut selectors::bloom::BloomFilter) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Property {
    FontFamily,
    FontSize,
    InitialLetter,
    Float,
    FontWeight,
    FontStyle,
    LineHeight,
    TextAlign,
    TextIndent,
    MarginTop,
    MarginRight,
    MarginBottom,
    MarginLeft,
    PaddingTop,
    PaddingRight,
    PaddingBottom,
    PaddingLeft,
    BorderTop,
    BorderRight,
    BorderBottom,
    BorderLeft,
    Width,
    MinWidth,
    MaxWidth,
    Height,
    MinHeight,
    MaxHeight,
    BreakBefore,
    BreakAfter,
    BreakInside,
    Widows,
    Orphans,
}

#[derive(Clone, Debug)]
enum Value {
    FontFamily(AuthorFontFamily),
    Ratio(u32),
    Weight(u16),
    Italic(bool),
    Align(TextAlignment),
    Length(CssLength),
    Border(CssBorder),
    Break(AuthorBreak),
    Avoid(bool),
    LineCount(u8),
    InitialLetter(Option<crate::model::flow::InitialLetter>),
    Float(crate::model::flow::AuthorFloat),
}

#[derive(Clone, Debug)]
struct Declaration {
    property: Property,
    value: Value,
    important: bool,
}

#[derive(Clone, Debug)]
struct Rule {
    selectors: SelectorList<EpubSelectorImpl>,
    declarations: Vec<Declaration>,
    order: u32,
}

struct SheetParser {
    next_order: u32,
}

impl<'i> AtRuleParser<'i> for SheetParser {
    type Prelude = bool;
    type AtRule = Vec<Rule>;
    type Error = SelectorParseErrorKind;

    fn parse_prelude(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<Self::Prelude, ParseError<Self::Error>> {
        if !name.eq_ignore_ascii_case("media") {
            return Err(ParseError::custom(
                SelectorParseErrorKind::UnsupportedPseudoClassOrElement,
            ));
        }
        let medium = input.expect_ident_cloned()?;
        input.expect_exhausted()?;
        Ok(matches!(
            medium.to_ascii_lowercase().as_str(),
            "all" | "screen" | "amzn-kf8"
        ))
    }

    fn parse_block(
        &mut self,
        enabled: Self::Prelude,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<Self::AtRule, ParseError<Self::Error>> {
        if !enabled {
            while input.next_including_whitespace_and_comments().is_ok() {}
            return Ok(Vec::new());
        }
        Ok(StyleSheetParser::new(input, self)
            .filter_map(Result::ok)
            .flatten()
            .collect())
    }
}

impl<'i> QualifiedRuleParser<'i> for SheetParser {
    type Prelude = SelectorList<EpubSelectorImpl>;
    type QualifiedRule = Vec<Rule>;
    type Error = SelectorParseErrorKind;

    fn parse_prelude(
        &mut self,
        input: &mut Parser<'i>,
    ) -> Result<Self::Prelude, ParseError<Self::Error>> {
        SelectorList::parse(&EpubSelectorParser, input, ParseRelative::No)
    }

    fn parse_block(
        &mut self,
        selectors: Self::Prelude,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<Self::QualifiedRule, ParseError<Self::Error>> {
        let mut declaration_parser = SupportedDeclarationParser;
        let declarations = RuleBodyParser::new(input, &mut declaration_parser)
            .filter_map(Result::ok)
            .flatten()
            .collect();
        let order = self.next_order;
        self.next_order = self.next_order.saturating_add(1);
        Ok(vec![Rule {
            selectors,
            declarations,
            order,
        }])
    }
}

struct SupportedDeclarationParser;

impl<'i> AtRuleParser<'i> for SupportedDeclarationParser {
    type Prelude = ();
    type AtRule = Vec<Declaration>;
    type Error = SelectorParseErrorKind;
}

impl<'i> QualifiedRuleParser<'i> for SupportedDeclarationParser {
    type Prelude = ();
    type QualifiedRule = Vec<Declaration>;
    type Error = SelectorParseErrorKind;
}

impl RuleBodyItemParser<'_, Vec<Declaration>, SelectorParseErrorKind>
    for SupportedDeclarationParser
{
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

impl<'i> DeclarationParser<'i> for SupportedDeclarationParser {
    type Declaration = Vec<Declaration>;
    type Error = SelectorParseErrorKind;

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _start: &ParserState,
    ) -> Result<Self::Declaration, ParseError<Self::Error>> {
        let start = input.position();
        while input.next_including_whitespace_and_comments().is_ok() {}
        let mut raw = input.slice_from(start).trim();
        let important = raw
            .get(raw.len().saturating_sub(10)..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case("!important"));
        if important {
            raw = raw[..raw.len() - 10].trim_end();
        }
        parse_declaration(&name, raw, important).ok_or_else(|| {
            ParseError::custom(SelectorParseErrorKind::UnsupportedPseudoClassOrElement)
        })
    }
}

fn parse_declaration(name: &str, raw: &str, important: bool) -> Option<Vec<Declaration>> {
    let lower = raw.trim().to_ascii_lowercase();
    let one = |property, value| {
        vec![Declaration {
            property,
            value,
            important,
        }]
    };
    let declarations = match name.to_ascii_lowercase().as_str() {
        "font-family" => {
            let first = raw.split(',').next()?.trim().trim_matches(['\'', '"']);
            let family = match first.to_ascii_lowercase().as_str() {
                "serif" => AuthorFontFamily::Serif,
                "sans-serif" | "sans serif" => AuthorFontFamily::SansSerif,
                "monospace" => AuthorFontFamily::Monospace,
                _ if !first.is_empty() => AuthorFontFamily::Named(first.to_owned()),
                _ => return None,
            };
            one(Property::FontFamily, Value::FontFamily(family))
        }
        "font-size" => one(Property::FontSize, Value::Ratio(parse_ratio(&lower)?)),
        "initial-letter" | "-webkit-initial-letter" => one(
            Property::InitialLetter,
            Value::InitialLetter(parse_initial_letter(&lower)?),
        ),
        "float" => one(
            Property::Float,
            Value::Float(match lower.as_str() {
                "none" => crate::model::flow::AuthorFloat::None,
                "left" => crate::model::flow::AuthorFloat::Left,
                "right" => crate::model::flow::AuthorFloat::Right,
                _ => return None,
            }),
        ),
        "font-weight" => {
            let weight = match lower.as_str() {
                "normal" => 400,
                "bold" => 700,
                _ => lower.parse::<u16>().ok()?.clamp(1, 1_000),
            };
            one(Property::FontWeight, Value::Weight(weight))
        }
        "font-style" => one(
            Property::FontStyle,
            Value::Italic(matches!(lower.as_str(), "italic" | "oblique")),
        ),
        "line-height" => one(Property::LineHeight, Value::Ratio(parse_ratio(&lower)?)),
        "text-align" => {
            let align = match lower.as_str() {
                "left" | "start" => TextAlignment::Left,
                "center" => TextAlignment::Center,
                "justify" => TextAlignment::Justify,
                "right" | "end" => TextAlignment::Right,
                _ => return None,
            };
            one(Property::TextAlign, Value::Align(align))
        }
        "text-indent" => one(Property::TextIndent, Value::Length(parse_length(&lower)?)),
        "margin-top" => one(Property::MarginTop, Value::Length(parse_length(&lower)?)),
        "margin-right" => one(Property::MarginRight, Value::Length(parse_length(&lower)?)),
        "margin-bottom" => one(Property::MarginBottom, Value::Length(parse_length(&lower)?)),
        "margin-left" => one(Property::MarginLeft, Value::Length(parse_length(&lower)?)),
        "margin" => box_declarations(
            [
                Property::MarginTop,
                Property::MarginRight,
                Property::MarginBottom,
                Property::MarginLeft,
            ],
            parse_box_lengths(&lower)?,
            important,
        ),
        "padding-top" => one(Property::PaddingTop, Value::Length(parse_length(&lower)?)),
        "padding-right" => one(Property::PaddingRight, Value::Length(parse_length(&lower)?)),
        "padding-bottom" => one(
            Property::PaddingBottom,
            Value::Length(parse_length(&lower)?),
        ),
        "padding-left" => one(Property::PaddingLeft, Value::Length(parse_length(&lower)?)),
        "padding" => box_declarations(
            [
                Property::PaddingTop,
                Property::PaddingRight,
                Property::PaddingBottom,
                Property::PaddingLeft,
            ],
            parse_box_lengths(&lower)?,
            important,
        ),
        "border-top" => one(Property::BorderTop, Value::Border(parse_border(&lower)?)),
        "border-right" => one(Property::BorderRight, Value::Border(parse_border(&lower)?)),
        "border-bottom" => one(Property::BorderBottom, Value::Border(parse_border(&lower)?)),
        "border-left" => one(Property::BorderLeft, Value::Border(parse_border(&lower)?)),
        "border" => {
            let border = parse_border(&lower)?;
            [
                Property::BorderTop,
                Property::BorderRight,
                Property::BorderBottom,
                Property::BorderLeft,
            ]
            .into_iter()
            .map(|property| Declaration {
                property,
                value: Value::Border(border),
                important,
            })
            .collect()
        }
        "width" => one(Property::Width, Value::Length(parse_length(&lower)?)),
        "min-width" => one(Property::MinWidth, Value::Length(parse_length(&lower)?)),
        "max-width" => one(Property::MaxWidth, Value::Length(parse_length(&lower)?)),
        "height" => one(Property::Height, Value::Length(parse_length(&lower)?)),
        "min-height" => one(Property::MinHeight, Value::Length(parse_length(&lower)?)),
        "max-height" => one(Property::MaxHeight, Value::Length(parse_length(&lower)?)),
        "page-break-before" | "break-before" => {
            one(Property::BreakBefore, Value::Break(parse_break(&lower)?))
        }
        "page-break-after" | "break-after" => {
            one(Property::BreakAfter, Value::Break(parse_break(&lower)?))
        }
        "page-break-inside" | "break-inside" => one(
            Property::BreakInside,
            Value::Avoid(matches!(lower.as_str(), "avoid" | "avoid-page")),
        ),
        "widows" => one(
            Property::Widows,
            Value::LineCount(lower.parse::<u8>().ok()?.max(1)),
        ),
        "orphans" => one(
            Property::Orphans,
            Value::LineCount(lower.parse::<u8>().ok()?.max(1)),
        ),
        _ => return None,
    };
    Some(declarations)
}

fn box_declarations(
    properties: [Property; 4],
    values: [CssLength; 4],
    important: bool,
) -> Vec<Declaration> {
    properties
        .into_iter()
        .zip(values)
        .map(|(property, value)| Declaration {
            property,
            value: Value::Length(value),
            important,
        })
        .collect()
}

fn parse_box_lengths(value: &str) -> Option<[CssLength; 4]> {
    let values = value
        .split_ascii_whitespace()
        .map(parse_length)
        .collect::<Option<Vec<_>>>()?;
    match values.as_slice() {
        [all] => Some([*all; 4]),
        [vertical, horizontal] => Some([*vertical, *horizontal, *vertical, *horizontal]),
        [top, horizontal, bottom] => Some([*top, *horizontal, *bottom, *horizontal]),
        [top, right, bottom, left] => Some([*top, *right, *bottom, *left]),
        _ => None,
    }
}

fn parse_border(value: &str) -> Option<CssBorder> {
    if value == "none" || value == "0" {
        return Some(CssBorder {
            width: CssLength::Px(0),
            color: None,
        });
    }
    let mut width = None;
    let mut color = None;
    let mut solid = false;
    for token in value.split_ascii_whitespace() {
        match token {
            "solid" => solid = true,
            "none" => {
                return Some(CssBorder {
                    width: CssLength::Px(0),
                    color: None,
                });
            }
            "thin" => width = Some(CssLength::Px(1_000)),
            "medium" => width = Some(CssLength::Px(3_000)),
            "thick" => width = Some(CssLength::Px(5_000)),
            _ if parse_length(token).is_some() => width = parse_length(token),
            _ if parse_color(token).is_some() => color = parse_color(token),
            _ => return None,
        }
    }
    solid.then_some(CssBorder {
        width: width.unwrap_or(CssLength::Px(3_000)),
        color,
    })
}

fn parse_color(value: &str) -> Option<CssColor> {
    let hex = value.strip_prefix('#')?;
    if !hex.is_ascii() {
        return None;
    }
    let (red, green, blue) = match hex.len() {
        3 => {
            let mut digits = hex
                .chars()
                .map(|digit| digit.to_digit(16).map(|value| value as u8));
            let red = digits.next()??;
            let green = digits.next()??;
            let blue = digits.next()??;
            (red * 17, green * 17, blue * 17)
        }
        6 => (
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    Some(CssColor { red, green, blue })
}

fn parse_break(value: &str) -> Option<AuthorBreak> {
    match value {
        "auto" => Some(AuthorBreak::Auto),
        "avoid" | "avoid-page" => Some(AuthorBreak::Avoid),
        "always" | "page" | "left" | "right" | "recto" | "verso" => Some(AuthorBreak::Page),
        _ => None,
    }
}

fn parse_initial_letter(value: &str) -> Option<Option<crate::model::flow::InitialLetter>> {
    if value == "normal" {
        return Some(None);
    }
    let parts = value.split_whitespace().collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    let (size_token, sink_token) = match parts.as_slice() {
        [size] => (*size, None),
        [keyword @ ("drop" | "raise"), size] => (*size, Some(*keyword)),
        [size, sink] => (*size, Some(*sink)),
        _ => return None,
    };
    let size = size_token.parse::<f32>().ok()?;
    if !size.is_finite() || size < 1.0 {
        return None;
    }
    let size_per_mille = (size * 1_000.0).round().clamp(1_000.0, u32::MAX as f32) as u32;
    let sink = match sink_token {
        None | Some("drop") => size.floor().clamp(1.0, u16::MAX as f32) as u16,
        Some("raise") => 1,
        Some(value) => value.parse::<u16>().ok().filter(|value| *value >= 1)?,
    };
    Some(Some(crate::model::flow::InitialLetter {
        size_per_mille,
        sink,
        legacy_float: false,
        margin_top: None,
        margin_right: None,
        margin_bottom: None,
        margin_left: None,
    }))
}

fn parse_ratio(value: &str) -> Option<u32> {
    let (number, factor) = if let Some(number) = value.strip_suffix("em") {
        (number, 1_000.0)
    } else if let Some(number) = value.strip_suffix('%') {
        (number, 10.0)
    } else {
        (value, 1_000.0)
    };
    let value = number.trim().parse::<f32>().ok()? * factor;
    (value.is_finite() && value > 0.0).then(|| value.round().clamp(100.0, 10_000.0) as u32)
}

fn parse_length(value: &str) -> Option<CssLength> {
    let value = value.trim();
    if value == "0" {
        return Some(CssLength::Px(0));
    }
    let (number, kind) = if let Some(number) = value.strip_suffix("em") {
        (number, 0)
    } else if let Some(number) = value.strip_suffix('%') {
        (number, 1)
    } else if let Some(number) = value.strip_suffix("px") {
        (number, 2)
    } else if let Some(number) = value.strip_suffix("pt") {
        (number, 3)
    } else {
        return None;
    };
    let fixed = (number.trim().parse::<f32>().ok()? * 1_000.0).round();
    if !fixed.is_finite() || fixed < i32::MIN as f32 || fixed > i32::MAX as f32 {
        return None;
    }
    Some(match kind {
        0 => CssLength::Em(fixed as i32),
        1 => CssLength::Percent(fixed as i32),
        2 => CssLength::Px(fixed as i32),
        _ => CssLength::Pt(fixed as i32),
    })
}

fn parse_stylesheet(source: &str, first_order: u32) -> Vec<Rule> {
    let mut input = Parser::new(source);
    let mut parser = SheetParser {
        next_order: first_order,
    };
    StyleSheetParser::new(&mut input, &mut parser)
        .filter_map(Result::ok)
        .flatten()
        .collect()
}

#[derive(Clone)]
struct Winner {
    important: bool,
    specificity: u32,
    order: u32,
    value: Value,
}

/// Cascades supported author declarations for every source node. Invalid rules
/// and unsupported declarations recover independently according to CSS syntax.
pub(super) fn cascade(tree: &XhtmlSourceTree, stylesheets: &[String]) -> Vec<AuthorStyle> {
    let mut rules = Vec::new();
    for source in stylesheets {
        let first_order = rules.len().try_into().unwrap_or(u32::MAX);
        rules.extend(parse_stylesheet(source, first_order));
    }
    let mut result = vec![AuthorStyle::default(); tree.nodes().len()];
    for (id, node) in tree.nodes() {
        if !matches!(node.kind(), XhtmlSourceNodeKind::Element(_)) {
            continue;
        }
        let element = TreeElement {
            tree,
            id,
            pseudo: false,
        };
        let mut winners = std::collections::HashMap::<Property, Winner>::new();
        let mut first_letter_winners = std::collections::HashMap::<Property, Winner>::new();
        for rule in &rules {
            for selector in rule.selectors.slice() {
                let pseudo = selector.pseudo_element();
                let target = if pseudo == Some(&EpubPseudoElement::FirstLetter) {
                    &mut first_letter_winners
                } else if pseudo.is_none() {
                    &mut winners
                } else {
                    continue;
                };
                let mut caches = SelectorCaches::default();
                let mut context = MatchingContext::new(
                    MatchingMode::Normal,
                    None,
                    &mut caches,
                    QuirksMode::NoQuirks,
                    NeedsSelectorFlags::No,
                    MatchingForInvalidation::No,
                );
                let matching_element = TreeElement {
                    pseudo: pseudo.is_some(),
                    ..element
                };
                if !matches_selector(selector, 0, None, matching_element, &mut context) {
                    continue;
                }
                let specificity = selector.specificity();
                for declaration in &rule.declarations {
                    let candidate = Winner {
                        important: declaration.important,
                        specificity,
                        order: rule.order,
                        value: declaration.value.clone(),
                    };
                    let replace = target.get(&declaration.property).is_none_or(|winner| {
                        (candidate.important, candidate.specificity, candidate.order)
                            >= (winner.important, winner.specificity, winner.order)
                    });
                    if replace {
                        target.insert(declaration.property, candidate);
                    }
                }
            }
        }
        let initial_letter_declared = winners.contains_key(&Property::InitialLetter);
        let style = &mut result[id.index()];
        for (property, winner) in winners {
            match (property, winner.value) {
                (Property::FontFamily, Value::FontFamily(value)) => style.font_family = Some(value),
                (Property::FontSize, Value::Ratio(value)) => {
                    style.font_size_per_mille = Some(value)
                }
                (Property::InitialLetter, Value::InitialLetter(value)) => {
                    style.initial_letter = value
                }
                (Property::Float, Value::Float(value)) => style.float = Some(value),
                (Property::FontWeight, Value::Weight(value)) => style.font_weight = Some(value),
                (Property::FontStyle, Value::Italic(value)) => style.italic = Some(value),
                (Property::LineHeight, Value::Ratio(value)) => {
                    style.line_height_per_mille = Some(value)
                }
                (Property::TextAlign, Value::Align(value)) => style.text_alignment = Some(value),
                (Property::TextIndent, Value::Length(value)) => style.text_indent = Some(value),
                (Property::MarginTop, Value::Length(value)) => style.margin_top = Some(value),
                (Property::MarginRight, Value::Length(value)) => style.margin_right = Some(value),
                (Property::MarginBottom, Value::Length(value)) => style.margin_bottom = Some(value),
                (Property::MarginLeft, Value::Length(value)) => style.margin_left = Some(value),
                (Property::PaddingTop, Value::Length(value)) => style.padding_top = Some(value),
                (Property::PaddingRight, Value::Length(value)) => style.padding_right = Some(value),
                (Property::PaddingBottom, Value::Length(value)) => {
                    style.padding_bottom = Some(value)
                }
                (Property::PaddingLeft, Value::Length(value)) => style.padding_left = Some(value),
                (Property::BorderTop, Value::Border(value)) => style.border_top = Some(value),
                (Property::BorderRight, Value::Border(value)) => style.border_right = Some(value),
                (Property::BorderBottom, Value::Border(value)) => style.border_bottom = Some(value),
                (Property::BorderLeft, Value::Border(value)) => style.border_left = Some(value),
                (Property::Width, Value::Length(value)) => style.width = Some(value),
                (Property::MinWidth, Value::Length(value)) => style.min_width = Some(value),
                (Property::MaxWidth, Value::Length(value)) => style.max_width = Some(value),
                (Property::Height, Value::Length(value)) => style.height = Some(value),
                (Property::MinHeight, Value::Length(value)) => style.min_height = Some(value),
                (Property::MaxHeight, Value::Length(value)) => style.max_height = Some(value),
                (Property::BreakBefore, Value::Break(value)) => style.break_before = Some(value),
                (Property::BreakAfter, Value::Break(value)) => style.break_after = Some(value),
                (Property::BreakInside, Value::Avoid(value)) => {
                    style.break_inside_avoid = Some(value)
                }
                (Property::Widows, Value::LineCount(value)) => style.widows = Some(value),
                (Property::Orphans, Value::LineCount(value)) => style.orphans = Some(value),
                _ => {}
            }
        }
        // Older Kindle authoring guidance expressed initial letters as an
        // enlarged left-floated inline. Preserve that declaration pattern in
        // the standards-shaped model; layout still requires it to be the
        // block's first content, so ordinary later floats are not reclassified.
        if style.initial_letter.is_none()
            && !initial_letter_declared
            && style.float == Some(crate::model::flow::AuthorFloat::Left)
            && style.font_size_per_mille.is_some_and(|size| size > 1_000)
        {
            let size_per_mille = style.font_size_per_mille.unwrap_or(1_000);
            style.initial_letter = Some(crate::model::flow::InitialLetter {
                size_per_mille,
                sink: (size_per_mille / 1_000).max(1).min(u32::from(u16::MAX)) as u16,
                legacy_float: true,
                margin_top: None,
                margin_right: None,
                margin_bottom: None,
                margin_left: None,
            });
        }
        if let Some(initial) = &mut style.initial_letter {
            initial.margin_top = style.margin_top;
            initial.margin_right = style.margin_right;
            initial.margin_bottom = style.margin_bottom;
            initial.margin_left = style.margin_left;
        }
        let pseudo_length = |property| {
            first_letter_winners
                .get(&property)
                .and_then(|winner| match winner.value {
                    Value::Length(value) => Some(value),
                    _ => None,
                })
        };
        if let Some(Winner {
            value: Value::InitialLetter(Some(mut initial)),
            ..
        }) = first_letter_winners.get(&Property::InitialLetter).cloned()
        {
            initial.margin_top = pseudo_length(Property::MarginTop);
            initial.margin_right = pseudo_length(Property::MarginRight);
            initial.margin_bottom = pseudo_length(Property::MarginBottom);
            initial.margin_left = pseudo_length(Property::MarginLeft);
            style.first_letter = Some(initial);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::super::EpubXhtmlProfile;
    use super::*;
    use crate::formats::xhtml::{XhtmlParseOptions, prepare_with_options};

    #[test]
    fn applies_specificity_source_order_inheritance_inputs_and_descendants() {
        let source = br#"<html xmlns="http://www.w3.org/1999/xhtml"><head/><body><div class="book"><p id="x" class="chapter">Title <span>inside</span></p></div></body></html>"#;
        let prepared = prepare_with_options(
            source,
            XhtmlParseOptions {
                allow_doctype: true,
            },
            &EpubXhtmlProfile,
        )
        .unwrap();
        let styles = cascade(
            &prepared.tree,
            &["p { font-size: 1em; font-weight: bold !important; text-indent: 20pt; } .chapter { font-size: 3em; font-weight: normal; } .chapter { font-size: 2em; text-align: center; } .book span { font-style: italic; } #x { margin-top: 10%; }".into()],
        );
        let paragraph = prepared
            .tree
            .nodes()
            .find(|(_, node)| matches!(node.kind(), XhtmlSourceNodeKind::Element(element) if element.name().local() == "p"))
            .unwrap()
            .0;
        assert_eq!(styles[paragraph.index()].font_size_per_mille, Some(2_000));
        assert_eq!(
            styles[paragraph.index()].text_alignment,
            Some(TextAlignment::Center)
        );
        assert_eq!(styles[paragraph.index()].font_weight, Some(700));
        assert_eq!(
            styles[paragraph.index()].text_indent,
            Some(CssLength::Pt(20_000))
        );
        assert!(
            (styles[paragraph.index()]
                .text_indent
                .unwrap()
                .resolve(16.0, 800.0)
                - 26.666_668)
                .abs()
                < 0.001
        );
        assert_eq!(
            styles[paragraph.index()].margin_top,
            Some(CssLength::Percent(10_000))
        );
        let span = prepared
            .tree
            .nodes()
            .find(|(_, node)| matches!(node.kind(), XhtmlSourceNodeKind::Element(element) if element.name().local() == "span"))
            .unwrap()
            .0;
        assert_eq!(styles[span.index()].italic, Some(true));
    }

    #[test]
    fn parses_standard_and_legacy_initial_letter_styles_without_class_names() {
        let source = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p><span id="standard">A</span>lpha</p><p><span id="legacy">B</span>eta</p><p id="pseudo">Charlie</p><p><span id="class-only" class="dropcap">D</span>elta</p></body></html>"#;
        let prepared = prepare_with_options(
            source,
            XhtmlParseOptions {
                allow_doctype: true,
            },
            &EpubXhtmlProfile,
        )
        .unwrap();
        let styles = cascade(
            &prepared.tree,
            &[
                "#standard { initial-letter: 2.5 2; margin-right: .2em; } #pseudo::first-letter { initial-letter: 7 2; margin-left: .1em; } @media amzn-kf8 { #legacy { font-size: 320%; float: left; margin-top: -.32em; } } @media amzn-mobi { #legacy { font-size: 9em; } }".into(),
            ],
        );
        let by_id = |wanted: &str| {
            prepared
                .tree
                .nodes()
                .find(|(_, node)| matches!(node.kind(), XhtmlSourceNodeKind::Element(element) if element.attributes().iter().any(|attribute| attribute.name().local() == "id" && attribute.value() == wanted)))
                .unwrap()
                .0
        };
        let standard = styles[by_id("standard").index()].initial_letter.unwrap();
        assert_eq!(standard.size_per_mille, 2_500);
        assert_eq!(standard.sink, 2);
        assert!(!standard.legacy_float);
        assert_eq!(standard.margin_right, Some(CssLength::Em(200)));

        let legacy_style = &styles[by_id("legacy").index()];
        let legacy = legacy_style.initial_letter.unwrap();
        assert_eq!(legacy_style.font_size_per_mille, Some(3_200));
        assert_eq!(legacy.sink, 3);
        assert!(legacy.legacy_float);
        assert_eq!(legacy.margin_top, Some(CssLength::Em(-320)));

        let pseudo = styles[by_id("pseudo").index()].first_letter.unwrap();
        assert_eq!(pseudo.size_per_mille, 7_000);
        assert_eq!(pseudo.sink, 2);
        assert_eq!(pseudo.margin_left, Some(CssLength::Em(100)));
        assert!(styles[by_id("class-only").index()].initial_letter.is_none());
    }

    #[test]
    fn cascades_epub_pagination_controls_and_legacy_aliases() {
        let source =
            br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p id="x">Text</p></body></html>"#;
        let prepared = prepare_with_options(
            source,
            XhtmlParseOptions {
                allow_doctype: true,
            },
            &EpubXhtmlProfile,
        )
        .unwrap();
        let styles = cascade(
            &prepared.tree,
            &["#x { margin: 1em 2em 3em 4em; padding: .5em 1em; border: 2px solid #bc1a1f; border-bottom: 1px solid #bfbfbf; width: 80%; min-height: 3em; page-break-before: always; page-break-after: avoid; break-inside: avoid-page; widows: 3; orphans: 4; }".into()],
        );
        let paragraph = prepared
            .tree
            .nodes()
            .find(|(_, node)| matches!(node.kind(), XhtmlSourceNodeKind::Element(element) if element.name().local() == "p"))
            .unwrap()
            .0;
        let style = &styles[paragraph.index()];
        assert_eq!(style.margin_left, Some(CssLength::Em(4_000)));
        assert_eq!(style.padding_top, Some(CssLength::Em(500)));
        assert_eq!(style.padding_right, Some(CssLength::Em(1_000)));
        assert_eq!(
            style.border_left,
            Some(CssBorder {
                width: CssLength::Px(2_000),
                color: Some(CssColor {
                    red: 0xbc,
                    green: 0x1a,
                    blue: 0x1f,
                }),
            })
        );
        assert_eq!(
            style.border_bottom,
            Some(CssBorder {
                width: CssLength::Px(1_000),
                color: Some(CssColor {
                    red: 0xbf,
                    green: 0xbf,
                    blue: 0xbf,
                }),
            })
        );
        assert_eq!(style.width, Some(CssLength::Percent(80_000)));
        assert_eq!(style.min_height, Some(CssLength::Em(3_000)));
        assert_eq!(style.break_before, Some(AuthorBreak::Page));
        assert_eq!(style.break_after, Some(AuthorBreak::Avoid));
        assert_eq!(style.break_inside_avoid, Some(true));
        assert_eq!(style.widows, Some(3));
        assert_eq!(style.orphans, Some(4));
    }
}
