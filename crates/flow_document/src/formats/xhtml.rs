//! Shared XHTML syntax support: strict XML parsing, names, source trees, and
//! validation profiles. Semantic import policy belongs to the format adapters;
//! shared lowering is an internal implementation detail.

use std::collections::HashSet;
use std::fmt;
use std::ops::Range;

use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{QName, ResolveResult};
use quick_xml::reader::NsReader;
use thiserror::Error;

pub(super) mod lowering;

const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";
const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_NODES: usize = 1_000_000;
const MAX_DEPTH: usize = 4_096;
const MAX_ATTRIBUTES_PER_ELEMENT: usize = 4_096;

/// Stable identity of a node in one temporary XHTML source tree.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceNodeId(u32);

impl SourceNodeId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A qualified XML name with its namespace resolved at the source location.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ExpandedName {
    qualified: String,
    prefix: Option<String>,
    local: String,
    namespace: Option<String>,
}

impl ExpandedName {
    pub fn qualified(&self) -> &str {
        &self.qualified
    }

    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_deref()
    }

    pub fn local(&self) -> &str {
        &self.local
    }

    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
}

/// One decoded attribute in source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XhtmlAttribute {
    name: ExpandedName,
    value: String,
    source_range: Range<usize>,
}

impl XhtmlAttribute {
    pub fn name(&self) -> &ExpandedName {
        &self.name
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    /// Range of the containing start tag. `quick-xml` does not expose a
    /// narrower attribute range, but retaining this range still gives repairs
    /// a deterministic source location.
    pub fn source_range(&self) -> Range<usize> {
        self.source_range.clone()
    }
}

/// Element data retained before validation, repair, and semantic lowering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XhtmlElement {
    name: ExpandedName,
    attributes: Vec<XhtmlAttribute>,
}

impl XhtmlElement {
    pub fn name(&self) -> &ExpandedName {
        &self.name
    }

    pub fn attributes(&self) -> &[XhtmlAttribute] {
        &self.attributes
    }

    pub fn attribute(&self, namespace: Option<&str>, local: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|attribute| {
                attribute.name.local() == local && attribute.name.namespace() == namespace
            })
            .map(XhtmlAttribute::value)
    }
}

/// Syntactic node kinds retained by the temporary source tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XhtmlSourceNodeKind {
    Document,
    Element(XhtmlElement),
    Text(String),
    Cdata(String),
    Comment(String),
    ProcessingInstruction { target: String, value: String },
}

/// One arena node in the temporary source tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XhtmlSourceNode {
    parent: Option<SourceNodeId>,
    children: Vec<SourceNodeId>,
    kind: XhtmlSourceNodeKind,
    source_range: Range<usize>,
}

impl XhtmlSourceNode {
    pub fn parent(&self) -> Option<SourceNodeId> {
        self.parent
    }

    pub fn children(&self) -> &[SourceNodeId] {
        &self.children
    }

    pub fn kind(&self) -> &XhtmlSourceNodeKind {
        &self.kind
    }

    pub fn source_range(&self) -> Range<usize> {
        self.source_range.clone()
    }
}

/// A flow-owned arena representation of an XML fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XhtmlSourceTree {
    nodes: Vec<XhtmlSourceNode>,
    root: SourceNodeId,
}

impl XhtmlSourceTree {
    pub fn root(&self) -> SourceNodeId {
        self.root
    }

    pub fn get(&self, id: SourceNodeId) -> Option<&XhtmlSourceNode> {
        self.nodes.get(id.index())
    }

    pub fn get_mut(&mut self, id: SourceNodeId) -> Option<&mut XhtmlSourceNode> {
        self.nodes.get_mut(id.index())
    }

    pub fn nodes(&self) -> impl ExactSizeIterator<Item = (SourceNodeId, &XhtmlSourceNode)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (SourceNodeId(index as u32), node))
    }

    fn push(
        &mut self,
        parent: SourceNodeId,
        kind: XhtmlSourceNodeKind,
        source_range: Range<usize>,
    ) -> Result<SourceNodeId, XhtmlParseError> {
        if self.nodes.len() >= MAX_NODES || self.nodes.len() > u32::MAX as usize {
            return Err(XhtmlParseError::TooComplex);
        }
        let id = SourceNodeId(self.nodes.len() as u32);
        self.nodes.push(XhtmlSourceNode {
            parent: Some(parent),
            children: Vec::new(),
            kind,
            source_range,
        });
        self.nodes[parent.index()].children.push(id);
        Ok(id)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct XhtmlParseOptions {
    /// Accept a document type declaration syntactically, but never load its
    /// external subset or use it to define entities.
    pub allow_doctype: bool,
}

#[derive(Debug, Error)]
pub enum XhtmlParseError {
    #[error("malformed XML at byte {offset}: {message}")]
    Malformed { offset: usize, message: String },
    #[error("XML document types are not accepted at byte {offset}")]
    Doctype { offset: usize },
    #[error("XHTML input exceeds the parser complexity limit")]
    TooComplex,
}

/// A diagnostic emitted by a validation/repair profile before lowering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XhtmlDiagnostic {
    pub code: &'static str,
    pub node: Option<SourceNodeId>,
    pub message: String,
}

/// Error reported by a validation/repair profile.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct XhtmlProfileError {
    pub message: String,
}

/// Format-specific validation and repair between syntax parsing and lowering.
pub trait XhtmlTreeProfile {
    fn apply(
        &self,
        tree: &mut XhtmlSourceTree,
        diagnostics: &mut Vec<XhtmlDiagnostic>,
    ) -> Result<(), XhtmlProfileError>;
}

/// Parsed and profile-processed XHTML ready for semantic lowering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedXhtml {
    pub tree: XhtmlSourceTree,
    pub diagnostics: Vec<XhtmlDiagnostic>,
}

#[derive(Debug, Error)]
pub enum XhtmlPrepareError {
    #[error(transparent)]
    Parse(#[from] XhtmlParseError),
    #[error(transparent)]
    Profile(#[from] XhtmlProfileError),
}

/// Parses bytes strictly, then runs the selected validation/repair profile.
pub fn prepare(
    source: &[u8],
    profile: &impl XhtmlTreeProfile,
) -> Result<PreparedXhtml, XhtmlPrepareError> {
    prepare_with_options(source, XhtmlParseOptions::default(), profile)
}

/// Parses bytes with explicit lexical policy, then applies a profile.
pub fn prepare_with_options(
    source: &[u8],
    options: XhtmlParseOptions,
    profile: &impl XhtmlTreeProfile,
) -> Result<PreparedXhtml, XhtmlPrepareError> {
    let mut tree = parse_with_options(source, options)?;
    let mut diagnostics = Vec::new();
    profile.apply(&mut tree, &mut diagnostics)?;
    Ok(PreparedXhtml { tree, diagnostics })
}

/// Parses an XML fragment without applying format-specific repair.
///
/// Multiple top-level elements become children of a synthetic document node.
/// Format adapters are responsible for document-shape validation.
pub fn parse(source: &[u8]) -> Result<XhtmlSourceTree, XhtmlParseError> {
    parse_with_options(source, XhtmlParseOptions::default())
}

pub fn parse_with_options(
    source: &[u8],
    options: XhtmlParseOptions,
) -> Result<XhtmlSourceTree, XhtmlParseError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(XhtmlParseError::TooComplex);
    }

    let mut reader = NsReader::from_reader(source);
    reader.config_mut().check_comments = true;
    reader.config_mut().check_end_names = true;
    reader.config_mut().trim_text(false);

    let root = SourceNodeId(0);
    let mut tree = XhtmlSourceTree {
        nodes: vec![XhtmlSourceNode {
            parent: None,
            children: Vec::new(),
            kind: XhtmlSourceNodeKind::Document,
            source_range: 0..source.len(),
        }],
        root,
    };
    let mut stack = vec![root];
    let mut buffer = Vec::new();
    let mut event_start = 0usize;

    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| malformed(event_start, error))?;
        let event_end = reader.buffer_position() as usize;
        let event_range = event_start..event_end;

        match event {
            Event::Start(start) => {
                if stack.len() > MAX_DEPTH {
                    return Err(XhtmlParseError::TooComplex);
                }
                let element = decode_element(&reader, &start, event_range.clone())?;
                let parent = *stack.last().expect("synthetic root remains on stack");
                let node = tree.push(parent, XhtmlSourceNodeKind::Element(element), event_range)?;
                stack.push(node);
            }
            Event::Empty(start) => {
                let element = decode_element(&reader, &start, event_range.clone())?;
                let parent = *stack.last().expect("synthetic root remains on stack");
                tree.push(parent, XhtmlSourceNodeKind::Element(element), event_range)?;
            }
            Event::End(_) => {
                let Some(node) = stack.pop() else {
                    return Err(malformed(event_start, "unexpected closing tag"));
                };
                if node == root {
                    return Err(malformed(event_start, "unexpected closing tag"));
                }
                tree.nodes[node.index()].source_range.end = event_end;
            }
            Event::Text(text) => {
                let value = text
                    .xml10_content()
                    .map_err(|error| malformed(event_start, error))?;
                validate_xml_chars(&value, event_start)?;
                append_text(&mut tree, *stack.last().unwrap(), &value, event_range)?;
            }
            Event::GeneralRef(reference) => {
                let value = if let Some(character) = reference
                    .resolve_char_ref()
                    .map_err(|error| malformed(event_start, error))?
                {
                    character.to_string()
                } else {
                    match reference
                        .xml10_content()
                        .map_err(|error| malformed(event_start, error))?
                        .as_ref()
                    {
                        "amp" => "&".into(),
                        "apos" => "'".into(),
                        "gt" => ">".into(),
                        "lt" => "<".into(),
                        "quot" => "\"".into(),
                        name => {
                            return Err(malformed(
                                event_start,
                                format!("unknown entity reference '&{name};'"),
                            ));
                        }
                    }
                };
                validate_xml_chars(&value, event_start)?;
                append_text(&mut tree, *stack.last().unwrap(), &value, event_range)?;
            }
            Event::CData(cdata) => {
                let value = cdata
                    .xml10_content()
                    .map_err(|error| malformed(event_start, error))?
                    .into_owned();
                validate_xml_chars(&value, event_start)?;
                let parent = *stack.last().unwrap();
                tree.push(parent, XhtmlSourceNodeKind::Cdata(value), event_range)?;
            }
            Event::Comment(comment) => {
                let value = comment
                    .xml10_content()
                    .map_err(|error| malformed(event_start, error))?
                    .into_owned();
                validate_xml_chars(&value, event_start)?;
                let parent = *stack.last().unwrap();
                tree.push(parent, XhtmlSourceNodeKind::Comment(value), event_range)?;
            }
            Event::PI(instruction) => {
                let target = reader
                    .decoder()
                    .decode(instruction.target())
                    .map_err(|error| malformed(event_start, error))?
                    .into_owned();
                validate_qualified_name(&target, event_start)?;
                let value = reader
                    .decoder()
                    .decode(instruction.content())
                    .map_err(|error| malformed(event_start, error))?;
                validate_xml_chars(&value, event_start)?;
                let parent = *stack.last().unwrap();
                tree.push(
                    parent,
                    XhtmlSourceNodeKind::ProcessingInstruction {
                        target,
                        value: normalize_xml10(value.as_ref()),
                    },
                    event_range,
                )?;
            }
            Event::Decl(_) => {}
            Event::DocType(doctype) => {
                if !options.allow_doctype {
                    return Err(XhtmlParseError::Doctype {
                        offset: event_start,
                    });
                }
                let value = doctype
                    .xml10_content()
                    .map_err(|error| malformed(event_start, error))?;
                validate_xml_chars(&value, event_start)?;
                // quick-xml does not load external subsets. Reject internal
                // subsets so accepting an EPUB doctype cannot introduce a
                // second entity-definition language into this parser.
                if value.contains('[') || value.contains(']') {
                    return Err(malformed(
                        event_start,
                        "internal document type subsets are not accepted",
                    ));
                }
            }
            Event::Eof => {
                if stack.len() != 1 {
                    return Err(malformed(source.len(), "unclosed element"));
                }
                break;
            }
        }

        event_start = event_end;
        buffer.clear();
    }

    Ok(tree)
}

fn decode_element(
    reader: &NsReader<&[u8]>,
    start: &BytesStart<'_>,
    source_range: Range<usize>,
) -> Result<XhtmlElement, XhtmlParseError> {
    let (namespace, _) = reader.resolver().resolve_element(start.name());
    let name = decode_name(reader, start.name(), namespace, source_range.start)?;
    let mut attributes = Vec::new();
    let mut expanded_attributes = HashSet::new();
    for attribute in start.attributes().with_checks(true) {
        if attributes.len() >= MAX_ATTRIBUTES_PER_ELEMENT {
            return Err(XhtmlParseError::TooComplex);
        }
        let attribute = attribute.map_err(|error| malformed(source_range.start, error))?;
        let qualified = decode_bytes(reader, attribute.key.as_ref(), source_range.start)?;
        validate_qualified_name(&qualified, source_range.start)?;
        let attribute_namespace = if qualified == "xmlns" || qualified.starts_with("xmlns:") {
            Some(XMLNS_NAMESPACE.to_owned())
        } else {
            let (namespace, _) = reader.resolver().resolve_attribute(attribute.key);
            decode_namespace(reader, namespace, source_range.start)?
        };
        let name = split_name(qualified, attribute_namespace);
        if name.namespace() != Some(XMLNS_NAMESPACE)
            && !expanded_attributes.insert((name.namespace.clone(), name.local.clone()))
        {
            return Err(malformed(
                source_range.start,
                format!("duplicate expanded attribute name '{}'", name.qualified()),
            ));
        }
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
            .map_err(|error| malformed(source_range.start, error))?;
        validate_xml_chars(&value, source_range.start)?;
        attributes.push(XhtmlAttribute {
            name,
            value: normalize_xml10(value.as_ref()),
            source_range: source_range.clone(),
        });
    }
    Ok(XhtmlElement { name, attributes })
}

fn decode_name(
    reader: &NsReader<&[u8]>,
    name: QName<'_>,
    namespace: ResolveResult<'_>,
    offset: usize,
) -> Result<ExpandedName, XhtmlParseError> {
    let qualified = decode_bytes(reader, name.as_ref(), offset)?;
    validate_qualified_name(&qualified, offset)?;
    let namespace = decode_namespace(reader, namespace, offset)?;
    Ok(split_name(qualified, namespace))
}

fn validate_xml_chars(value: &str, offset: usize) -> Result<(), XhtmlParseError> {
    if let Some(character) = value.chars().find(|character| {
        !matches!(*character, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
    }) {
        return Err(malformed(
            offset,
            format!("character U+{:04X} is not legal in XML 1.0", character as u32),
        ));
    }
    Ok(())
}

fn validate_qualified_name(name: &str, offset: usize) -> Result<(), XhtmlParseError> {
    let mut parts = name.split(':');
    let first = parts.next().unwrap_or_default();
    let second = parts.next();
    if first.is_empty()
        || !is_xml_name_start(first.chars().next().unwrap())
        || !first.chars().skip(1).all(is_xml_name_continue)
        || second.is_some_and(|part| {
            part.is_empty()
                || !is_xml_name_start(part.chars().next().unwrap())
                || !part.chars().skip(1).all(is_xml_name_continue)
        })
        || parts.next().is_some()
    {
        return Err(malformed(
            offset,
            format!("invalid qualified XML name '{name}'"),
        ));
    }
    Ok(())
}

fn is_xml_name_start(character: char) -> bool {
    matches!(
        character,
        'A'..='Z'
            | '_'
            | 'a'..='z'
            | '\u{c0}'..='\u{d6}'
            | '\u{d8}'..='\u{f6}'
            | '\u{f8}'..='\u{2ff}'
            | '\u{370}'..='\u{37d}'
            | '\u{37f}'..='\u{1fff}'
            | '\u{200c}'..='\u{200d}'
            | '\u{2070}'..='\u{218f}'
            | '\u{2c00}'..='\u{2fef}'
            | '\u{3001}'..='\u{d7ff}'
            | '\u{f900}'..='\u{fdcf}'
            | '\u{fdf0}'..='\u{fffd}'
            | '\u{10000}'..='\u{effff}'
    )
}

fn is_xml_name_continue(character: char) -> bool {
    is_xml_name_start(character)
        || matches!(
            character,
            '-' | '.' | '0'..='9' | '\u{b7}' | '\u{300}'..='\u{36f}' | '\u{203f}'..='\u{2040}'
        )
}

fn split_name(qualified: String, namespace: Option<String>) -> ExpandedName {
    let (prefix, local) = qualified
        .split_once(':')
        .map_or((None, qualified.as_str()), |(prefix, local)| {
            (Some(prefix.to_owned()), local)
        });
    ExpandedName {
        local: local.to_owned(),
        qualified,
        prefix,
        namespace,
    }
}

fn decode_namespace(
    reader: &NsReader<&[u8]>,
    namespace: ResolveResult<'_>,
    offset: usize,
) -> Result<Option<String>, XhtmlParseError> {
    match namespace {
        ResolveResult::Unbound => Ok(None),
        ResolveResult::Bound(namespace) => {
            decode_bytes(reader, namespace.as_ref(), offset).map(Some)
        }
        ResolveResult::Unknown(prefix) => Err(malformed(
            offset,
            format!(
                "namespace prefix '{}' is not bound",
                String::from_utf8_lossy(&prefix)
            ),
        )),
    }
}

fn decode_bytes(
    reader: &NsReader<&[u8]>,
    bytes: &[u8],
    offset: usize,
) -> Result<String, XhtmlParseError> {
    reader
        .decoder()
        .decode(bytes)
        .map(|value| value.into_owned())
        .map_err(|error| malformed(offset, error))
}

fn append_text(
    tree: &mut XhtmlSourceTree,
    parent: SourceNodeId,
    value: &str,
    source_range: Range<usize>,
) -> Result<(), XhtmlParseError> {
    if value.is_empty() {
        return Ok(());
    }
    if let Some(last) = tree.nodes[parent.index()].children.last().copied()
        && let XhtmlSourceNodeKind::Text(existing) = &mut tree.nodes[last.index()].kind
    {
        existing.push_str(value);
        tree.nodes[last.index()].source_range.end = source_range.end;
        return Ok(());
    }
    tree.push(
        parent,
        XhtmlSourceNodeKind::Text(value.to_owned()),
        source_range,
    )?;
    Ok(())
}

fn normalize_xml10(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn malformed(offset: usize, error: impl fmt::Display) -> XhtmlParseError {
    XhtmlParseError::Malformed {
        offset,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_namespaces_attributes_order_and_source_identity() {
        let source = br#"<x:section xmlns:x="urn:example" xml:lang="en" bare="yes"><x:p>text</x:p></x:section>"#;
        let tree = parse(source).unwrap();
        let document = tree.get(tree.root()).unwrap();
        let section_id = document.children()[0];
        let section = tree.get(section_id).unwrap();
        let XhtmlSourceNodeKind::Element(section_element) = section.kind() else {
            panic!("expected section element");
        };
        assert_eq!(section_element.name().qualified(), "x:section");
        assert_eq!(section_element.name().prefix(), Some("x"));
        assert_eq!(section_element.name().local(), "section");
        assert_eq!(section_element.name().namespace(), Some("urn:example"));
        assert_eq!(
            section_element.attributes()[0].name().qualified(),
            "xmlns:x"
        );
        assert_eq!(
            section_element.attributes()[0].name().namespace(),
            Some(XMLNS_NAMESPACE)
        );
        assert_eq!(
            section_element.attributes()[1].name().namespace(),
            Some("http://www.w3.org/XML/1998/namespace")
        );
        assert_eq!(section_element.attribute(None, "bare"), Some("yes"));
        assert_eq!(section.parent(), Some(tree.root()));
        assert_eq!(section.source_range(), 0..source.len());
    }

    #[test]
    fn preserves_text_then_normalizes_xml_line_endings() {
        let tree = parse(b"<p>a\r\nb\rc &amp; d &#x2014; e</p>").unwrap();
        let paragraph = tree
            .get(tree.get(tree.root()).unwrap().children()[0])
            .unwrap();
        let text = tree.get(paragraph.children()[0]).unwrap();
        assert_eq!(
            text.kind(),
            &XhtmlSourceNodeKind::Text("a\nb\nc & d — e".into())
        );
    }

    #[test]
    fn retains_cdata_comments_and_processing_instructions_separately() {
        let tree = parse(b"<?audit keep?><p>a<![CDATA[<b>]]><!-- note -->z</p>").unwrap();
        let document = tree.get(tree.root()).unwrap();
        assert!(matches!(
            tree.get(document.children()[0]).unwrap().kind(),
            XhtmlSourceNodeKind::ProcessingInstruction { target, value }
                if target == "audit" && value == " keep"
        ));
        let paragraph = tree.get(document.children()[1]).unwrap();
        assert!(matches!(
            tree.get(paragraph.children()[1]).unwrap().kind(),
            XhtmlSourceNodeKind::Cdata(value) if value == "<b>"
        ));
        assert!(matches!(
            tree.get(paragraph.children()[2]).unwrap().kind(),
            XhtmlSourceNodeKind::Comment(value) if value == " note "
        ));
    }

    #[test]
    fn rejects_lexically_invalid_xml_unknown_namespaces_and_doctypes() {
        for source in [
            b"<p><strong>broken</p>".as_slice(),
            b"<p title='one' title='two'/>".as_slice(),
            b"<1p/>".as_slice(),
            b"<x:p/>".as_slice(),
            b"<p xmlns:a='urn:x' xmlns:b='urn:x' a:v='1' b:v='2'/>".as_slice(),
            b"<p>&unknown;</p>".as_slice(),
            b"<p>\0</p>".as_slice(),
        ] {
            assert!(
                parse(source).is_err(),
                "accepted {}",
                String::from_utf8_lossy(source)
            );
        }
        assert!(matches!(
            parse(b"<!DOCTYPE p><p>text</p>"),
            Err(XhtmlParseError::Doctype { .. })
        ));
    }

    #[test]
    fn fragments_may_have_multiple_top_level_nodes() {
        let tree = parse(b"lead<p>one</p><p>two</p>tail").unwrap();
        assert_eq!(tree.get(tree.root()).unwrap().children().len(), 4);
    }
}
