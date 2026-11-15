//! Bounded, memory-backed EPUB OCF/OPF parsing and materialization.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::sync::Arc;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use thiserror::Error;
use zip::CompressionMethod;

mod model;

use flow_document::formats::epub_xhtml::{self, EpubXhtmlOptions, XhtmlImportError};
use flow_document::model::flow::{DocumentReference, ImportOutcome, SourceRetention};

const EPUB_MIMETYPE: &[u8] = b"application/epub+zip";
const PACKAGE_MEDIA_TYPE: &str = "application/oebps-package+xml";
const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_DECLARED_UNCOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_CONTAINER_BYTES: usize = 1024 * 1024;
const MAX_PACKAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESOURCE_BYTES: usize = 64 * 1024 * 1024;

/// Common OPF metadata retained at publication scope.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PublicationMetadata {
    pub titles: Vec<String>,
    pub creators: Vec<String>,
    pub languages: Vec<String>,
    pub identifiers: Vec<MetadataIdentifier>,
    pub publishers: Vec<String>,
    pub descriptions: Vec<String>,
    pub dates: Vec<String>,
    pub rights: Vec<String>,
}

/// An OPF identifier and its optional XML ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataIdentifier {
    pub id: Option<String>,
    pub value: String,
}

/// One package manifest record. `href` is normalized from the OCF root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestItem {
    pub id: String,
    pub href: String,
    pub media_type: String,
    pub properties: Vec<String>,
    pub fallback: Option<String>,
}

/// One ordered OPF spine reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpineItem {
    pub item_id: String,
    pub href: String,
    pub media_type: String,
    pub linear: bool,
    pub properties: Vec<String>,
}

/// One OPF 2 guide reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuideReference {
    pub kind: String,
    pub title: Option<String>,
    pub href: String,
    pub fragment: Option<String>,
}

/// One hierarchical NCX or EPUB 3 navigation point.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NavigationPoint {
    pub id: Option<String>,
    pub label: String,
    pub href: String,
    pub fragment: Option<String>,
    pub play_order: Option<u32>,
    pub children: Vec<NavigationPoint>,
}

/// Recoverable package compatibility findings kept separate from semantic flow diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EpubDiagnostic {
    /// OCF requires the first `mimetype` entry to be stored, but some deployed
    /// publications deflate it. The entry remains bounded and unambiguous.
    DeflatedMimetype,
}

/// A validated packed EPUB with immutable archive bytes and package metadata.
#[derive(Clone)]
pub struct EpubPublication {
    archive: Arc<[u8]>,
    entries: BTreeSet<String>,
    revision: String,
    diagnostics: Vec<EpubDiagnostic>,
    package_path: String,
    version: String,
    unique_identifier: Option<String>,
    toc_item_id: Option<String>,
    metadata: PublicationMetadata,
    manifest: BTreeMap<String, ManifestItem>,
    manifest_by_href: BTreeMap<String, String>,
    spine: Vec<SpineItem>,
    guide: Vec<GuideReference>,
    navigation: Vec<NavigationPoint>,
}

impl std::fmt::Debug for EpubPublication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EpubPublication")
            .field("archive_bytes", &self.archive.len())
            .field("entries", &self.entries.len())
            .field("revision", &self.revision)
            .field("diagnostics", &self.diagnostics)
            .field("package_path", &self.package_path)
            .field("version", &self.version)
            .field("unique_identifier", &self.unique_identifier)
            .field("toc_item_id", &self.toc_item_id)
            .field("metadata", &self.metadata)
            .field("manifest_items", &self.manifest.len())
            .field("spine_items", &self.spine.len())
            .field("guide", &self.guide)
            .field("navigation_points", &self.navigation.len())
            .finish()
    }
}

impl EpubPublication {
    /// Opens and validates a packed EPUB from resident OCF bytes.
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> Result<Self, EpubError> {
        let archive = bytes.into();
        let _span = flow_document::telemetry::phase_span!(
            "flow.publication.parse",
            format = "epub",
            source_bytes = archive.len() as u64,
        );
        if archive.len() > MAX_ARCHIVE_BYTES {
            return Err(EpubError::Limit("archive exceeds the byte limit"));
        }

        let inspection = inspect_archive(&archive)?;
        let entries = inspection.entries;
        let diagnostics = inspection.diagnostics;
        if inspection.mimetype != EPUB_MIMETYPE {
            return Err(EpubError::Invalid("invalid EPUB mimetype"));
        }

        let container = read_entry(
            &archive,
            &entries,
            "META-INF/container.xml",
            MAX_CONTAINER_BYTES,
        )?;
        let package_path = parse_container(&container)?;
        let package_source = read_entry(&archive, &entries, &package_path, MAX_PACKAGE_BYTES)?;
        let parsed = parse_package(&package_source, &package_path)?;
        for item in parsed.manifest.values() {
            if !entries.contains(&item.href) {
                return Err(EpubError::MissingEntry(item.href.clone()));
            }
            if let Some(fallback) = &item.fallback
                && !parsed.manifest.contains_key(fallback)
            {
                return Err(EpubError::InvalidOwned(format!(
                    "manifest item '{}' names unknown fallback '{fallback}'",
                    item.id
                )));
            }
        }
        for reference in &parsed.guide {
            if !entries.contains(&reference.href) {
                return Err(EpubError::MissingEntry(reference.href.clone()));
            }
        }
        let epub3_nav = parsed
            .manifest
            .values()
            .find(|item| item.properties.iter().any(|property| property == "nav"));
        let navigation = if let Some(nav) = epub3_nav {
            if nav.media_type != "application/xhtml+xml" {
                return Err(EpubError::Invalid(
                    "EPUB 3 navigation item is not an XHTML document",
                ));
            }
            let source = read_entry(&archive, &entries, &nav.href, MAX_PACKAGE_BYTES)?;
            parse_epub3_navigation(&source, &nav.href, &parsed.manifest_by_href)?
        } else if let Some(toc_id) = &parsed.toc_item_id {
            let toc = &parsed.manifest[toc_id];
            if toc.media_type != "application/x-dtbncx+xml" {
                return Err(EpubError::Invalid("spine toc item is not an NCX document"));
            }
            let source = read_entry(&archive, &entries, &toc.href, MAX_PACKAGE_BYTES)?;
            parse_ncx(&source, &toc.href, &parsed.manifest_by_href)?
        } else {
            Vec::new()
        };
        let revision = blake3::hash(&archive).to_hex().to_string();

        flow_document::telemetry::phase_event!(
            "flow.phase.complete",
            entries = entries.len() as u64,
            spine_items = parsed.spine.len() as u64,
            navigation_points = navigation.len() as u64,
        );
        Ok(Self {
            archive,
            entries,
            revision,
            diagnostics,
            package_path,
            version: parsed.version,
            unique_identifier: parsed.unique_identifier,
            toc_item_id: parsed.toc_item_id,
            metadata: parsed.metadata,
            manifest: parsed.manifest,
            manifest_by_href: parsed.manifest_by_href,
            spine: parsed.spine,
            guide: parsed.guide,
            navigation,
        })
    }

    /// Content revision used to invalidate publication and resource caches.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn diagnostics(&self) -> &[EpubDiagnostic] {
        &self.diagnostics
    }

    pub fn package_path(&self) -> &str {
        &self.package_path
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn unique_identifier(&self) -> Option<&str> {
        self.unique_identifier.as_deref()
    }

    pub fn toc_item_id(&self) -> Option<&str> {
        self.toc_item_id.as_deref()
    }

    pub fn metadata(&self) -> &PublicationMetadata {
        &self.metadata
    }

    pub fn manifest(&self) -> &BTreeMap<String, ManifestItem> {
        &self.manifest
    }

    pub fn spine(&self) -> &[SpineItem] {
        &self.spine
    }

    pub fn guide(&self) -> &[GuideReference] {
        &self.guide
    }

    pub fn navigation(&self) -> &[NavigationPoint] {
        &self.navigation
    }

    /// Reads a bounded, manifest-declared package resource.
    pub fn resource(&self, item_id: &str) -> Result<Vec<u8>, EpubError> {
        let item = self
            .manifest
            .get(item_id)
            .ok_or_else(|| EpubError::UnknownItem(item_id.to_owned()))?;
        read_entry(&self.archive, &self.entries, &item.href, MAX_RESOURCE_BYTES)
    }

    /// Resolves and reads a manifest resource relative to a package document.
    /// External URLs are left to the host and return `Ok(None)`.
    pub fn resource_relative_to(
        &self,
        document_href: &str,
        url: &str,
    ) -> Result<Option<(&ManifestItem, Vec<u8>)>, EpubError> {
        if has_uri_scheme(url) || url.starts_with("//") {
            return Ok(None);
        }
        let href = resolve_href(document_href, url, true)?;
        let item_id = self
            .manifest_by_href
            .get(&href)
            .ok_or_else(|| EpubError::UndeclaredResource(href.clone()))?;
        let item = &self.manifest[item_id];
        let bytes = read_entry(&self.archive, &self.entries, &href, MAX_RESOURCE_BYTES)?;
        Ok(Some((item, bytes)))
    }

    /// Resolves a stable manifest ID or normalized package href to its spine index.
    pub fn spine_index(&self, identity: &str) -> Option<usize> {
        self.spine
            .iter()
            .position(|item| item.item_id == identity || item.href == identity)
    }

    /// Imports a spine item by manifest ID or package href.
    pub fn import_spine_identity(
        &self,
        identity: &str,
        retention: SourceRetention,
    ) -> Result<ImportOutcome, EpubError> {
        self.import_spine_item(
            self.spine_index(identity)
                .ok_or_else(|| EpubError::UnknownSpineIdentity(identity.into()))?,
            retention,
        )
    }

    pub fn import_spine_item(
        &self,
        index: usize,
        retention: SourceRetention,
    ) -> Result<ImportOutcome, EpubError> {
        let item = self.spine.get(index).ok_or(EpubError::SpineIndex(index))?;
        let item = self
            .manifest
            .get(&item.item_id)
            .ok_or_else(|| EpubError::UnknownItem(item.item_id.clone()))?;
        self.import_item(item, retention)
    }

    fn import_item(
        &self,
        item: &ManifestItem,
        retention: SourceRetention,
    ) -> Result<ImportOutcome, EpubError> {
        let _span = flow_document::telemetry::phase_span!("flow.publication.item", format = "epub");
        if item.media_type != "application/xhtml+xml" && item.media_type != "text/html" {
            return Err(EpubError::UnsupportedSpineMediaType {
                href: item.href.clone(),
                media_type: item.media_type.clone(),
            });
        }

        let source = read_entry(&self.archive, &self.entries, &item.href, MAX_CONTENT_BYTES)?;
        let source =
            std::str::from_utf8(&source).map_err(|_| EpubError::NonUtf8(item.href.clone()))?;
        let mut stylesheets = Vec::new();
        for href in epub_xhtml::stylesheet_hrefs(source).map_err(XhtmlImportError::Prepare)? {
            if has_uri_scheme(&href) || href.starts_with("//") {
                continue;
            }
            let stylesheet_path = resolve_href(&item.href, &href, true)?;
            let manifest_id = self
                .manifest_by_href
                .get(&stylesheet_path)
                .ok_or_else(|| EpubError::UndeclaredResource(stylesheet_path.clone()))?;
            let stylesheet_item = &self.manifest[manifest_id];
            if stylesheet_item.media_type != "text/css" {
                return Err(EpubError::InvalidOwned(format!(
                    "linked stylesheet '{}' has media type '{}'",
                    stylesheet_item.href, stylesheet_item.media_type
                )));
            }
            let bytes = read_entry(
                &self.archive,
                &self.entries,
                &stylesheet_item.href,
                MAX_PACKAGE_BYTES,
            )?;
            stylesheets.push(
                String::from_utf8(bytes)
                    .map_err(|_| EpubError::NonUtf8(stylesheet_item.href.clone()))?,
            );
        }

        let result = epub_xhtml::import(
            source,
            EpubXhtmlOptions {
                resource_base: item
                    .href
                    .rsplit_once('/')
                    .map(|(parent, _)| format!("{parent}/")),
                identity: Some(DocumentReference::PublicationItem(item.href.clone())),
                retention,
                stylesheets,
            },
        )
        .map_err(EpubError::from);
        #[cfg(feature = "telemetry")]
        if let Ok(outcome) = &result {
            flow_document::telemetry::phase_event!(
                "flow.phase.complete",
                canonical_bytes = outcome.document.text().len().as_usize() as u64,
            );
        }
        result
    }
}

#[derive(Debug, Error)]
pub enum EpubError {
    #[error(transparent)]
    Model(#[from] crate::publication::ModelError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Xhtml(#[from] XhtmlImportError),
    #[error("invalid EPUB: {0}")]
    Invalid(&'static str),
    #[error("invalid EPUB: {0}")]
    InvalidOwned(String),
    #[error("EPUB limit exceeded: {0}")]
    Limit(&'static str),
    #[error("missing OCF entry '{0}'")]
    MissingEntry(String),
    #[error("unknown manifest item '{0}'")]
    UnknownItem(String),
    #[error("undeclared package resource '{0}'")]
    UndeclaredResource(String),
    #[error("spine index {0} is out of range")]
    SpineIndex(usize),
    #[error("unknown spine identity '{0}'")]
    UnknownSpineIdentity(String),
    #[error("spine item '{href}' has unsupported media type '{media_type}'")]
    UnsupportedSpineMediaType { href: String, media_type: String },
    #[error("package resource '{0}' is not UTF-8")]
    NonUtf8(String),
}

struct ArchiveInspection {
    entries: BTreeSet<String>,
    mimetype: Vec<u8>,
    diagnostics: Vec<EpubDiagnostic>,
}

fn inspect_archive(bytes: &[u8]) -> Result<ArchiveInspection, EpubError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    if archive.is_empty() {
        return Err(EpubError::Invalid("empty OCF archive"));
    }
    if archive.len() > MAX_ENTRIES {
        return Err(EpubError::Limit("archive has too many entries"));
    }

    let mut diagnostics = Vec::new();
    let mimetype = {
        let mut first = archive.by_index(0)?;
        if first.name_raw() != b"mimetype" {
            return Err(EpubError::Invalid("mimetype must be the first OCF entry"));
        }
        if first.compression() != CompressionMethod::Stored {
            diagnostics.push(EpubDiagnostic::DeflatedMimetype);
        }
        read_zip_file(&mut first, EPUB_MIMETYPE.len())?
    };

    let mut entries = BTreeSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        if file.encrypted() {
            return Err(EpubError::Invalid("encrypted OCF entries are unsupported"));
        }
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(EpubError::Invalid(
                "symbolic links are not valid OCF resources",
            ));
        }
        let raw_name = std::str::from_utf8(file.name_raw())
            .map_err(|_| EpubError::Invalid("OCF entry name is not UTF-8"))?;
        let name = normalize_archive_name(raw_name)?;
        if !entries.insert(name) {
            return Err(EpubError::Invalid("duplicate normalized OCF entry"));
        }
        total = total
            .checked_add(file.size())
            .ok_or(EpubError::Limit("declared archive size overflow"))?;
        if total > MAX_DECLARED_UNCOMPRESSED_BYTES {
            return Err(EpubError::Limit(
                "declared uncompressed archive is too large",
            ));
        }
    }
    Ok(ArchiveInspection {
        entries,
        mimetype,
        diagnostics,
    })
}

fn read_entry(
    bytes: &[u8],
    entries: &BTreeSet<String>,
    path: &str,
    limit: usize,
) -> Result<Vec<u8>, EpubError> {
    if !entries.contains(path) {
        return Err(EpubError::MissingEntry(path.to_owned()));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    let mut file = archive.by_name(path)?;
    read_zip_file(&mut file, limit)
}

fn read_zip_file(file: &mut impl Read, limit: usize) -> Result<Vec<u8>, EpubError> {
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(EpubError::Limit("expanded entry exceeds its byte limit"));
    }
    Ok(bytes)
}

fn parse_container(source: &[u8]) -> Result<String, EpubError> {
    let mut reader = Reader::from_reader(source);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut roots = Vec::new();
    let mut saw_container = false;
    loop {
        match reader.read_event_into(&mut buffer).map_err(xml_error)? {
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"container" =>
            {
                saw_container = true;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"rootfile" =>
            {
                let attrs = attributes(&reader, &element)?;
                if let Some(path) = attrs.get("full-path") {
                    roots.push((path.clone(), attrs.get("media-type").cloned()));
                }
            }
            Event::DocType(_) => return Err(EpubError::Invalid("OCF XML doctypes are forbidden")),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !saw_container {
        return Err(EpubError::Invalid("container.xml has no container element"));
    }
    let (path, _) = roots
        .iter()
        .find(|(_, media)| media.as_deref() == Some(PACKAGE_MEDIA_TYPE))
        .or_else(|| roots.first())
        .ok_or(EpubError::Invalid("container.xml has no rootfile"))?;
    normalize_root_path(path)
}

struct PendingNavigationPoint {
    id: Option<String>,
    label: String,
    href: Option<String>,
    fragment: Option<String>,
    play_order: Option<u32>,
    children: Vec<NavigationPoint>,
}

fn parse_ncx(
    source: &[u8],
    ncx_path: &str,
    manifest_by_href: &BTreeMap<String, String>,
) -> Result<Vec<NavigationPoint>, EpubError> {
    let mut reader = Reader::from_reader(source);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut roots = Vec::new();
    let mut stack = Vec::<PendingNavigationPoint>::new();
    let mut in_nav_label = false;
    let mut label = None::<String>;
    let mut saw_ncx = false;
    let mut saw_nav_map = false;

    loop {
        match reader.read_event_into(&mut buffer).map_err(xml_error)? {
            Event::Start(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                let attrs = attributes(&reader, &element)?;
                match name {
                    b"ncx" => saw_ncx = true,
                    b"navMap" => saw_nav_map = true,
                    b"navPoint" => {
                        if stack.len() >= 128 {
                            return Err(EpubError::Limit("NCX hierarchy is too deep"));
                        }
                        stack.push(PendingNavigationPoint {
                            id: attrs.get("id").cloned(),
                            label: String::new(),
                            href: None,
                            fragment: None,
                            play_order: attrs
                                .get("playOrder")
                                .map(|value| {
                                    value.parse().map_err(|_| {
                                        EpubError::Invalid("invalid NCX playOrder value")
                                    })
                                })
                                .transpose()?,
                            children: Vec::new(),
                        });
                    }
                    b"navLabel" if !stack.is_empty() => in_nav_label = true,
                    b"text" if in_nav_label => label = Some(String::new()),
                    b"content" if !stack.is_empty() => {
                        set_ncx_content(
                            stack.last_mut().unwrap(),
                            ncx_path,
                            manifest_by_href,
                            required_attr(&attrs, "src")?,
                        )?;
                    }
                    _ => {}
                }
            }
            Event::Empty(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                if name == b"content" && !stack.is_empty() {
                    let attrs = attributes(&reader, &element)?;
                    set_ncx_content(
                        stack.last_mut().unwrap(),
                        ncx_path,
                        manifest_by_href,
                        required_attr(&attrs, "src")?,
                    )?;
                }
            }
            Event::Text(text) => {
                if let Some(label) = &mut label {
                    label.push_str(&text.xml10_content().map_err(xml_error)?);
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(label) = &mut label {
                    if let Some(character) = reference.resolve_char_ref().map_err(xml_error)? {
                        label.push(character);
                    } else {
                        let name = reference.xml10_content().map_err(xml_error)?;
                        label.push_str(match name.as_ref() {
                            "amp" => "&",
                            "apos" => "'",
                            "gt" => ">",
                            "lt" => "<",
                            "quot" => "\"",
                            _ => return Err(EpubError::Invalid("unknown NCX entity reference")),
                        });
                    }
                }
            }
            Event::End(element) => {
                let qualified = element.name();
                match local_name(qualified.as_ref()) {
                    b"text" if label.is_some() => {
                        stack.last_mut().unwrap().label = label.take().unwrap().trim().to_owned();
                    }
                    b"navLabel" => in_nav_label = false,
                    b"navPoint" => {
                        let pending = stack
                            .pop()
                            .ok_or(EpubError::Invalid("unbalanced NCX navPoint"))?;
                        let href = pending
                            .href
                            .ok_or(EpubError::Invalid("NCX navPoint has no content"))?;
                        let point = NavigationPoint {
                            id: pending.id,
                            label: if pending.label.is_empty() {
                                href.clone()
                            } else {
                                pending.label
                            },
                            href,
                            fragment: pending.fragment,
                            play_order: pending.play_order,
                            children: pending.children,
                        };
                        if let Some(parent) = stack.last_mut() {
                            parent.children.push(point);
                        } else {
                            roots.push(point);
                        }
                    }
                    _ => {}
                }
            }
            Event::DocType(doctype) => {
                if doctype.xml10_content().map_err(xml_error)?.contains('[') {
                    return Err(EpubError::Invalid("NCX internal DTD subsets are forbidden"));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !saw_ncx || !saw_nav_map || !stack.is_empty() {
        return Err(EpubError::Invalid("NCX has no complete navigation map"));
    }
    Ok(roots)
}

struct PendingEpub3NavigationPoint {
    id: Option<String>,
    label: String,
    raw_href: Option<String>,
    children: Vec<NavigationPoint>,
}

struct Epub3LabelCapture {
    depth: usize,
    text: String,
}

fn parse_epub3_navigation(
    source: &[u8],
    nav_path: &str,
    manifest_by_href: &BTreeMap<String, String>,
) -> Result<Vec<NavigationPoint>, EpubError> {
    let mut reader = Reader::from_reader(source);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut toc_depth = None;
    let mut stack = Vec::<PendingEpub3NavigationPoint>::new();
    let mut capture = None::<Epub3LabelCapture>;
    let mut roots = Vec::new();
    let mut point_count = 0_usize;

    loop {
        match reader.read_event_into(&mut buffer).map_err(xml_error)? {
            Event::Start(element) => {
                depth += 1;
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                let attrs = attributes(&reader, &element)?;
                if name == b"nav"
                    && toc_depth.is_none()
                    && (words(attrs.get("type")).iter().any(|value| value == "toc")
                        || attrs.get("role").is_some_and(|role| role == "doc-toc"))
                {
                    toc_depth = Some(depth);
                } else if toc_depth.is_some_and(|nav_depth| depth > nav_depth) {
                    match name {
                        b"li" => {
                            if stack.len() >= 128 {
                                return Err(EpubError::Limit(
                                    "EPUB 3 navigation hierarchy is too deep",
                                ));
                            }
                            point_count += 1;
                            if point_count > MAX_ENTRIES {
                                return Err(EpubError::Limit(
                                    "EPUB 3 navigation has too many entries",
                                ));
                            }
                            stack.push(PendingEpub3NavigationPoint {
                                id: attrs.get("id").cloned(),
                                label: String::new(),
                                raw_href: None,
                                children: Vec::new(),
                            });
                        }
                        b"a" | b"span" if !stack.is_empty() && capture.is_none() => {
                            if name == b"a" {
                                stack.last_mut().unwrap().raw_href =
                                    Some(required_attr(&attrs, "href")?.to_owned());
                            }
                            capture = Some(Epub3LabelCapture {
                                depth,
                                text: String::new(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            Event::Empty(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                if toc_depth.is_some_and(|nav_depth| depth >= nav_depth)
                    && name == b"a"
                    && !stack.is_empty()
                {
                    let attrs = attributes(&reader, &element)?;
                    stack.last_mut().unwrap().raw_href =
                        Some(required_attr(&attrs, "href")?.to_owned());
                }
            }
            Event::Text(text) => {
                if let Some(capture) = &mut capture {
                    capture
                        .text
                        .push_str(&text.xml10_content().map_err(xml_error)?);
                }
            }
            Event::CData(text) => {
                if let Some(capture) = &mut capture {
                    capture
                        .text
                        .push_str(&text.xml10_content().map_err(xml_error)?);
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(capture) = &mut capture {
                    if let Some(character) = reference.resolve_char_ref().map_err(xml_error)? {
                        capture.text.push(character);
                    } else {
                        let name = reference.xml10_content().map_err(xml_error)?;
                        capture.text.push_str(match name.as_ref() {
                            "amp" => "&",
                            "apos" => "'",
                            "gt" => ">",
                            "lt" => "<",
                            "quot" => "\"",
                            _ => {
                                return Err(EpubError::Invalid(
                                    "unknown EPUB 3 navigation entity reference",
                                ));
                            }
                        });
                    }
                }
            }
            Event::End(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                if capture
                    .as_ref()
                    .is_some_and(|capture| capture.depth == depth)
                {
                    let label = normalize_navigation_label(&capture.take().unwrap().text);
                    if !label.is_empty() {
                        stack.last_mut().unwrap().label = label;
                    }
                }
                if name == b"li" && toc_depth.is_some_and(|nav_depth| depth > nav_depth) {
                    let pending = stack
                        .pop()
                        .ok_or(EpubError::Invalid("unbalanced EPUB 3 navigation list"))?;
                    let point = finish_epub3_navigation_point(pending, nav_path, manifest_by_href)?;
                    if let Some(point) = point {
                        if let Some(parent) = stack.last_mut() {
                            parent.children.push(point);
                        } else {
                            roots.push(point);
                        }
                    }
                }
                if name == b"nav" && toc_depth == Some(depth) {
                    toc_depth = None;
                }
                depth = depth.saturating_sub(1);
            }
            Event::DocType(doctype) => {
                if doctype.xml10_content().map_err(xml_error)?.contains('[') {
                    return Err(EpubError::Invalid(
                        "EPUB 3 navigation internal DTD subsets are forbidden",
                    ));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !stack.is_empty() || capture.is_some() {
        return Err(EpubError::Invalid(
            "EPUB 3 navigation has unbalanced list content",
        ));
    }
    if roots.is_empty() {
        return Err(EpubError::Invalid(
            "EPUB 3 publication has no TOC navigation",
        ));
    }
    Ok(roots)
}

fn finish_epub3_navigation_point(
    pending: PendingEpub3NavigationPoint,
    nav_path: &str,
    manifest_by_href: &BTreeMap<String, String>,
) -> Result<Option<NavigationPoint>, EpubError> {
    let (href, fragment) = if let Some(raw_href) = pending.raw_href.as_deref() {
        resolve_navigation_destination(nav_path, manifest_by_href, raw_href)?
    } else if let Some(first) = pending.children.first() {
        (first.href.clone(), first.fragment.clone())
    } else {
        return Ok(None);
    };
    Ok(Some(NavigationPoint {
        id: pending.id,
        label: if pending.label.is_empty() {
            href.clone()
        } else {
            pending.label
        },
        href,
        fragment,
        play_order: None,
        children: pending.children,
    }))
}

fn normalize_navigation_label(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn resolve_navigation_destination(
    base_path: &str,
    manifest_by_href: &BTreeMap<String, String>,
    source: &str,
) -> Result<(String, Option<String>), EpubError> {
    let (path, fragment) = if let Some((path, fragment)) = source.split_once('#') {
        (path, Some(percent_decode(fragment)?))
    } else {
        (source, None)
    };
    let path = if path.is_empty() {
        base_path.to_owned()
    } else {
        resolve_href(base_path, path, false)?
    };
    if !manifest_by_href.contains_key(&path) {
        return Err(EpubError::UndeclaredResource(path));
    }
    Ok((path, fragment))
}

fn set_ncx_content(
    point: &mut PendingNavigationPoint,
    ncx_path: &str,
    manifest_by_href: &BTreeMap<String, String>,
    source: &str,
) -> Result<(), EpubError> {
    let (href, fragment) = resolve_navigation_destination(ncx_path, manifest_by_href, source)?;
    point.href = Some(href);
    point.fragment = fragment;
    Ok(())
}

#[derive(Default)]
struct ParsedPackage {
    version: String,
    unique_identifier: Option<String>,
    toc_item_id: Option<String>,
    metadata: PublicationMetadata,
    manifest: BTreeMap<String, ManifestItem>,
    manifest_by_href: BTreeMap<String, String>,
    spine: Vec<SpineItem>,
    guide: Vec<GuideReference>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Section {
    None,
    Metadata,
    Manifest,
    Spine,
    Guide,
}

fn parse_package(source: &[u8], package_path: &str) -> Result<ParsedPackage, EpubError> {
    let mut reader = Reader::from_reader(source);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut parsed = ParsedPackage::default();
    let mut section = Section::None;
    let mut raw_spine = Vec::<(String, bool, Vec<String>)>::new();
    let mut metadata_value: Option<(String, Option<String>, String)> = None;
    let mut unique_identifier_id = None;
    let mut saw_package = false;

    loop {
        match reader.read_event_into(&mut buffer).map_err(xml_error)? {
            Event::Start(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                let attrs = attributes(&reader, &element)?;
                match name {
                    b"package" => {
                        saw_package = true;
                        parsed.version = attrs.get("version").cloned().unwrap_or_default();
                        unique_identifier_id = attrs.get("unique-identifier").cloned();
                    }
                    b"metadata" => section = Section::Metadata,
                    b"manifest" => section = Section::Manifest,
                    b"spine" => {
                        section = Section::Spine;
                        parsed.toc_item_id = attrs.get("toc").cloned();
                    }
                    b"guide" => section = Section::Guide,
                    _ if section == Section::Metadata && is_metadata_name(name) => {
                        metadata_value = Some((
                            String::from_utf8_lossy(name).into_owned(),
                            attrs.get("id").cloned(),
                            String::new(),
                        ));
                    }
                    _ => {}
                }
                if name == b"item" && section == Section::Manifest {
                    add_manifest_item(&mut parsed, package_path, &attrs)?;
                } else if name == b"itemref" && section == Section::Spine {
                    add_raw_spine(&mut raw_spine, &attrs)?;
                } else if name == b"reference" && section == Section::Guide {
                    add_guide_reference(&mut parsed.guide, package_path, &attrs)?;
                }
            }
            Event::Empty(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                let attrs = attributes(&reader, &element)?;
                if name == b"item" && section == Section::Manifest {
                    add_manifest_item(&mut parsed, package_path, &attrs)?;
                } else if name == b"itemref" && section == Section::Spine {
                    add_raw_spine(&mut raw_spine, &attrs)?;
                } else if name == b"reference" && section == Section::Guide {
                    add_guide_reference(&mut parsed.guide, package_path, &attrs)?;
                }
            }
            Event::Text(text) => {
                if let Some((_, _, value)) = &mut metadata_value {
                    value.push_str(&text.xml10_content().map_err(xml_error)?);
                }
            }
            Event::CData(text) => {
                if let Some((_, _, value)) = &mut metadata_value {
                    value.push_str(&text.xml10_content().map_err(xml_error)?);
                }
            }
            Event::GeneralRef(reference) => {
                if let Some((_, _, value)) = &mut metadata_value {
                    if let Some(character) = reference.resolve_char_ref().map_err(xml_error)? {
                        value.push(character);
                    } else {
                        let name = reference.xml10_content().map_err(xml_error)?;
                        value.push_str(match name.as_ref() {
                            "amp" => "&",
                            "apos" => "'",
                            "gt" => ">",
                            "lt" => "<",
                            "quot" => "\"",
                            _ => return Err(EpubError::Invalid("unknown OPF entity reference")),
                        });
                    }
                }
            }
            Event::End(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if metadata_value
                    .as_ref()
                    .is_some_and(|(field, _, _)| field.as_bytes() == name)
                {
                    let (field, id, value) = metadata_value.take().unwrap();
                    push_metadata(&mut parsed.metadata, &field, id, value);
                }
                match name {
                    b"metadata" | b"manifest" | b"spine" | b"guide" => section = Section::None,
                    _ => {}
                }
            }
            Event::DocType(_) => return Err(EpubError::Invalid("OPF doctypes are forbidden")),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }

    if !saw_package || parsed.version.is_empty() {
        return Err(EpubError::Invalid("OPF package has no package version"));
    }
    if parsed.manifest.is_empty() || raw_spine.is_empty() {
        return Err(EpubError::Invalid("OPF manifest or spine is empty"));
    }
    parsed.unique_identifier = unique_identifier_id.and_then(|wanted| {
        parsed
            .metadata
            .identifiers
            .iter()
            .find(|identifier| identifier.id.as_deref() == Some(&wanted))
            .map(|identifier| identifier.value.clone())
    });
    if let Some(toc) = &parsed.toc_item_id
        && !parsed.manifest.contains_key(toc)
    {
        return Err(EpubError::Invalid(
            "spine toc does not name a manifest item",
        ));
    }
    for (item_id, linear, properties) in raw_spine {
        let item = parsed.manifest.get(&item_id).ok_or_else(|| {
            EpubError::InvalidOwned(format!("spine references unknown item '{item_id}'"))
        })?;
        parsed.spine.push(SpineItem {
            item_id,
            href: item.href.clone(),
            media_type: item.media_type.clone(),
            linear,
            properties,
        });
    }
    Ok(parsed)
}

fn add_manifest_item(
    parsed: &mut ParsedPackage,
    package_path: &str,
    attrs: &BTreeMap<String, String>,
) -> Result<(), EpubError> {
    let id = required_attr(attrs, "id")?.to_owned();
    let href = resolve_href(package_path, required_attr(attrs, "href")?, false)?;
    let media_type = required_attr(attrs, "media-type")?.to_owned();
    let item = ManifestItem {
        id: id.clone(),
        href: href.clone(),
        media_type,
        properties: words(attrs.get("properties")),
        fallback: attrs.get("fallback").cloned(),
    };
    if parsed.manifest.insert(id.clone(), item).is_some() {
        return Err(EpubError::Invalid("duplicate OPF manifest ID"));
    }
    if parsed.manifest_by_href.insert(href, id).is_some() {
        return Err(EpubError::Invalid("duplicate normalized manifest href"));
    }
    Ok(())
}

fn add_raw_spine(
    spine: &mut Vec<(String, bool, Vec<String>)>,
    attrs: &BTreeMap<String, String>,
) -> Result<(), EpubError> {
    let idref = required_attr(attrs, "idref")?.to_owned();
    let linear = attrs.get("linear").is_none_or(|value| value != "no");
    spine.push((idref, linear, words(attrs.get("properties"))));
    Ok(())
}

fn add_guide_reference(
    guide: &mut Vec<GuideReference>,
    package_path: &str,
    attrs: &BTreeMap<String, String>,
) -> Result<(), EpubError> {
    let kind = required_attr(attrs, "type")?.to_owned();
    let raw_href = required_attr(attrs, "href")?;
    let (path, fragment) = if let Some((path, fragment)) = raw_href.split_once('#') {
        (path, Some(percent_decode(fragment)?))
    } else {
        (raw_href, None)
    };
    guide.push(GuideReference {
        kind,
        title: attrs.get("title").cloned(),
        href: resolve_href(package_path, path, false)?,
        fragment,
    });
    Ok(())
}

fn push_metadata(
    metadata: &mut PublicationMetadata,
    field: &str,
    id: Option<String>,
    value: String,
) {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return;
    }
    match field {
        "title" => metadata.titles.push(value),
        "creator" => metadata.creators.push(value),
        "language" => metadata.languages.push(value),
        "identifier" => metadata.identifiers.push(MetadataIdentifier { id, value }),
        "publisher" => metadata.publishers.push(value),
        "description" => metadata.descriptions.push(value),
        "date" => metadata.dates.push(value),
        "rights" => metadata.rights.push(value),
        _ => {}
    }
}

fn attributes(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
) -> Result<BTreeMap<String, String>, EpubError> {
    let mut result = BTreeMap::new();
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| EpubError::InvalidOwned(error.to_string()))?;
        let key = String::from_utf8_lossy(local_name(attribute.key.as_ref())).into_owned();
        let value = attribute
            .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())
            .map_err(xml_error)?
            .into_owned();
        result.insert(key, value);
    }
    Ok(result)
}

fn required_attr<'a>(
    attrs: &'a BTreeMap<String, String>,
    name: &'static str,
) -> Result<&'a str, EpubError> {
    attrs
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(EpubError::InvalidOwned(format!(
            "missing OPF attribute '{name}'"
        )))
}

fn words(value: Option<&String>) -> Vec<String> {
    value
        .map(|value| value.split_ascii_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn is_metadata_name(name: &[u8]) -> bool {
    matches!(
        name,
        b"title"
            | b"creator"
            | b"language"
            | b"identifier"
            | b"publisher"
            | b"description"
            | b"date"
            | b"rights"
    )
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

fn xml_error(error: impl std::fmt::Display) -> EpubError {
    EpubError::InvalidOwned(format!("malformed package XML: {error}"))
}

fn normalize_archive_name(path: &str) -> Result<String, EpubError> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return Err(EpubError::Invalid("unsafe OCF entry name"));
    }
    let directory = path.ends_with('/');
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" if directory => {}
            "" | "." | ".." => return Err(EpubError::Invalid("non-canonical OCF entry name")),
            _ => parts.push(part),
        }
    }
    let normalized = parts.join("/");
    if normalized.is_empty() {
        return Err(EpubError::Invalid("empty OCF entry name"));
    }
    Ok(normalized)
}

fn normalize_root_path(path: &str) -> Result<String, EpubError> {
    normalize_segments(std::iter::empty(), path)
}

fn resolve_href(base_file: &str, href: &str, allow_suffix: bool) -> Result<String, EpubError> {
    if href.is_empty() || has_uri_scheme(href) || href.starts_with("//") {
        return Err(EpubError::Invalid("package href is not a local OCF path"));
    }
    let href = if allow_suffix {
        href.split(['?', '#']).next().unwrap_or(href)
    } else {
        if href.contains(['?', '#']) {
            return Err(EpubError::Invalid(
                "package href contains a query or fragment",
            ));
        }
        href
    };
    let base = base_file.rsplit_once('/').map_or("", |(base, _)| base);
    normalize_segments(base.split('/').filter(|part| !part.is_empty()), href)
}

fn normalize_segments<'a>(
    base: impl Iterator<Item = &'a str>,
    path: &str,
) -> Result<String, EpubError> {
    let path = percent_decode(path)?;
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return Err(EpubError::Invalid("unsafe OCF path"));
    }
    let mut parts: Vec<String> = base.map(str::to_owned).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(EpubError::Invalid("OCF path escapes the archive root"));
                }
            }
            _ => parts.push(part.to_owned()),
        }
    }
    if parts.is_empty() {
        return Err(EpubError::Invalid("empty normalized OCF path"));
    }
    Ok(parts.join("/"))
}

fn percent_decode(value: &str) -> Result<String, EpubError> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex(*bytes
                .get(index + 1)
                .ok_or(EpubError::Invalid("bad percent escape"))?)?;
            let low = hex(*bytes
                .get(index + 2)
                .ok_or(EpubError::Invalid("bad percent escape"))?)?;
            output.push(high << 4 | low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).map_err(|_| EpubError::Invalid("OCF path is not UTF-8"))
}

fn hex(byte: u8) -> Result<u8, EpubError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(EpubError::Invalid("bad percent escape")),
    }
}

fn has_uri_scheme(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;

    fn fixture(mimetype_first: bool, deflate_mimetype: bool) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut add = |name: &str, contents: &[u8], options| {
            writer.start_file(name, options).unwrap();
            writer.write_all(contents).unwrap();
        };
        if mimetype_first {
            add(
                "mimetype",
                EPUB_MIMETYPE,
                if deflate_mimetype { deflated } else { stored },
            );
        }
        add(
            "META-INF/container.xml",
            br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            deflated,
        );
        add(
            "OPS/package.opf",
            br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="book-id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Small Book</dc:title><dc:creator>A. Reader</dc:creator><dc:language>en</dc:language><dc:identifier id="book-id">urn:test:book</dc:identifier></metadata><manifest><item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/><item id="style" href="styles/book.css" media-type="text/css"/><item id="image" href="images/pixel.png" media-type="image/png"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/></manifest><spine toc="ncx"><itemref idref="chapter"/></spine><guide><reference type="text" title="Start" href="text/chapter.xhtml#start"/></guide></package>"#,
            deflated,
        );
        add(
            "OPS/text/chapter.xhtml",
            br#"<?xml version="1.0"?><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd"><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter</title><link rel="stylesheet" href="../styles/book.css"/></head><body><p id="start" class="opening"><span class="ornamental-initial">P</span>acked EPUB text.</p></body></html>"#,
            deflated,
        );
        add(
            "OPS/styles/book.css",
            b"p.opening { text-align: center; font-style: italic; } @media amzn-kf8 { span.ornamental-initial { font-size: 320%; float: left; margin-top: -0.32em; margin-bottom: -0.32em; } }",
            deflated,
        );
        add("OPS/images/pixel.png", b"synthetic-image", deflated);
        add(
            "OPS/toc.ncx",
            br#"<ncx><navMap><navPoint id="chapter" playOrder="1"><navLabel><text>Small Chapter</text></navLabel><content src="text/chapter.xhtml#start"/></navPoint></navMap></ncx>"#,
            deflated,
        );
        if !mimetype_first {
            add("mimetype", EPUB_MIMETYPE, stored);
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn opens_opf2_and_imports_a_styled_spine_document() {
        let publication = EpubPublication::from_bytes(fixture(true, false)).unwrap();
        assert_eq!(publication.package_path(), "OPS/package.opf");
        assert_eq!(publication.version(), "2.0");
        assert_eq!(publication.unique_identifier(), Some("urn:test:book"));
        assert_eq!(publication.metadata().titles, ["Small Book"]);
        assert_eq!(publication.spine()[0].href, "OPS/text/chapter.xhtml");
        assert_eq!(publication.guide()[0].fragment.as_deref(), Some("start"));
        assert_eq!(publication.navigation()[0].label, "Small Chapter");
        assert_eq!(publication.navigation()[0].href, "OPS/text/chapter.xhtml");
        assert_eq!(
            publication.navigation()[0].fragment.as_deref(),
            Some("start")
        );
        let (resource, bytes) = publication
            .resource_relative_to("OPS/text/chapter.xhtml", "../images/pixel.png")
            .unwrap()
            .unwrap();
        assert_eq!(resource.id, "image");
        assert_eq!(bytes, b"synthetic-image");

        assert_eq!(publication.spine_index("chapter"), Some(0));
        assert_eq!(publication.spine_index("OPS/text/chapter.xhtml"), Some(0));
        let outcome = publication
            .import_spine_identity("chapter", SourceRetention::Discard)
            .unwrap();
        assert_eq!(outcome.document.text().as_str(), "Packed EPUB text.");
        let resource_base = "OPS/text/";
        assert_eq!(outcome.document.resource_base(), Some(resource_base));
        assert!(outcome.document.style_inputs().any(|(_, style)| {
            style.author().is_some_and(|author| {
                author.text_alignment == Some(flow_document::model::flow::TextAlignment::Center)
                    && author.italic == Some(true)
            })
        }));
        assert!(outcome.document.style_inputs().any(|(_, style)| {
            style.author().is_some_and(|author| {
                author.font_size_per_mille == Some(3_200)
                    && author
                        .initial_letter
                        .is_some_and(|initial| initial.sink == 3)
            })
        }));
    }

    #[test]
    fn parses_epub3_toc_navigation_in_preference_to_other_navs() {
        let manifest = BTreeMap::from([
            ("OPS/nav.xhtml".to_owned(), "nav".to_owned()),
            ("OPS/text/one.xhtml".to_owned(), "one".to_owned()),
            ("OPS/text/two.xhtml".to_owned(), "two".to_owned()),
        ]);
        let navigation = parse_epub3_navigation(
            br#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="landmarks"><ol><li><a href="text/one.xhtml">Ignored</a></li></ol></nav><nav epub:type="toc"><h1>Contents</h1><ol><li id="part"><span> Part One </span><ol><li><a href="text/one.xhtml#start"> First <em>chapter</em> </a></li><li><a href="text/two.xhtml">Second chapter</a></li></ol></li></ol></nav></body></html>"#,
            "OPS/nav.xhtml",
            &manifest,
        )
        .unwrap();
        assert_eq!(navigation.len(), 1);
        assert_eq!(navigation[0].id.as_deref(), Some("part"));
        assert_eq!(navigation[0].label, "Part One");
        assert_eq!(navigation[0].href, "OPS/text/one.xhtml");
        assert_eq!(navigation[0].fragment.as_deref(), Some("start"));
        assert_eq!(navigation[0].children.len(), 2);
        assert_eq!(navigation[0].children[0].label, "First chapter");
        assert_eq!(navigation[0].children[1].href, "OPS/text/two.xhtml");
        assert_eq!(navigation[0].children[0].play_order, None);
    }

    #[test]
    fn diagnoses_a_deflated_but_first_mimetype_entry() {
        let publication = EpubPublication::from_bytes(fixture(true, true)).unwrap();
        assert_eq!(
            publication.diagnostics(),
            [EpubDiagnostic::DeflatedMimetype]
        );
    }

    #[test]
    fn rejects_nonconforming_mimetype_order_and_escaping_paths() {
        assert!(matches!(
            EpubPublication::from_bytes(fixture(false, false)),
            Err(EpubError::Invalid(_))
        ));
        assert!(resolve_href("OPS/package.opf", "../../outside", false).is_err());
        assert!(normalize_root_path("/absolute").is_err());
    }
}
