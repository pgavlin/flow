//! Host-owned input, adapter selection, resource policy and decoding.
use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use flow_publication::{
    document::{
        formats::{flow_xhtml, markdown, mediawiki_html, web_html},
        model::flow::{Destination, DocumentReference, SourceRetention},
        viewer::{FontResource, ImagePixels},
    },
    formats::epub::EpubPublication,
    publication::{
        DocumentDescriptor, DocumentKey, ImportedDocument, Publication, PublicationDescription,
        PublicationKey, PublicationMetadata, PublicationRevision, PublicationTarget,
        ResourceReference,
    },
    reading_session::{
        DocumentRequest, NavigationTarget, PreparedResource, ResolvedLink, ResourceKind,
        ResourceRequest,
    },
};
use std::{io::Cursor, sync::Arc};
use url::Url;

#[cfg(not(target_arch = "wasm32"))]
mod http;
#[cfg(not(target_arch = "wasm32"))]
use http::{EncodedResources, http_client};
mod wikipedia;
#[cfg(target_arch = "wasm32")]
pub use wikipedia::UrlRequest;
pub use wikipedia::url_request;

pub const MAX_INPUT: usize = 64 * 1024 * 1024;
const MAX_DOCUMENT: usize = 16 * 1024 * 1024;
const MAX_RESOURCE: usize = 16 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;

pub const WELCOME: &str = "# flow demo\n\nOpen a Markdown document, web HTML, MediaWiki/Parsoid DOM (.mw.html), flow semantic XHTML, or EPUB. Pass an HTTP(S) URL on the desktop command line, or use the browser URL field (requires CORS). Wikipedia article URLs automatically request Parsoid HTML; other web pages use article extraction.\n\n- **Ctrl/Cmd+O**: open (or drop a file on the desktop window)\n- **P**: toggle continuous / paginated reading\n- **Wheel, Space, Page Up/Down**: scroll or turn pages\n- **Left/Right**: previous/next page (or EPUB document in continuous mode)\n- **Alt+Left** or **Backspace**: back\n- **Ctrl/Cmd + / −**: font size\n\nClick document links to follow them. This demo has no library, persistence, or settings screen. Desktop web documents can fetch HTTP(S) images; browser URL import currently loads article text but not remote images.\n\nIn a browser, use the Open button. EPUB images and fonts are self-contained; an uploaded standalone file cannot read neighboring files on your disk.";

pub struct Book {
    pub title: String,
    pub model: Publication,
    input: Input,
    #[cfg(not(target_arch = "wasm32"))]
    encoded_resources: std::cell::RefCell<EncodedResources>,
}
enum Input {
    Epub(EpubPublication),
    Single {
        document: ImportedDocument,
        web_url: Option<Url>,
        #[cfg(not(target_arch = "wasm32"))]
        path: Option<std::path::PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ContentFormat {
    Epub,
    Markdown,
    FlowXhtml,
    WebHtml,
    MediaWikiHtml(Option<mediawiki_html::MediaWikiHtmlVersion>),
}

impl ContentFormat {
    fn for_content_type(value: &str) -> Result<Self> {
        match value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "application/epub+zip" => Ok(Self::Epub),
            "text/markdown" | "text/x-markdown" | "text/plain" => Ok(Self::Markdown),
            "text/html" | "application/xhtml+xml" => {
                let mut version = None;
                // Only an explicit Parsoid media profile selects the wiki adapter.
                // Do not infer it from a host name, URL suffix, or body markup.
                for parameter in value.split(';').skip(1) {
                    let Some((key, profiles)) = parameter.trim().split_once('=') else {
                        continue;
                    };
                    if !key.trim().eq_ignore_ascii_case("profile") {
                        continue;
                    }
                    for profile in profiles.trim().trim_matches('"').split_ascii_whitespace() {
                        let declared = [
                            "https://www.mediawiki.org/wiki/Specs/HTML/",
                            "http://www.mediawiki.org/wiki/Specs/HTML/",
                        ]
                        .into_iter()
                        .find_map(|prefix| profile.strip_prefix(prefix));
                        if let Some(declared) = declared {
                            let declared = declared
                                .parse()
                                .context("Invalid Parsoid Content-Type profile")?;
                            ensure!(
                                version.is_none_or(|previous| previous == declared),
                                "Conflicting Parsoid Content-Type profiles"
                            );
                            version = Some(declared);
                        }
                    }
                }
                Ok(version.map_or(Self::WebHtml, |version| Self::MediaWikiHtml(Some(version))))
            }
            _ => bail!("Unsupported response Content-Type: {value}"),
        }
    }
}

impl Book {
    pub fn from_bytes(name: &str, bytes: Vec<u8>, generation: u64) -> Result<Self> {
        let extension = name
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase());
        let format = match extension.as_deref() {
            Some("epub") => ContentFormat::Epub,
            Some("md" | "markdown") => ContentFormat::Markdown,
            Some("xhtml") => ContentFormat::FlowXhtml,
            Some("html" | "htm") => {
                let name = name.to_ascii_lowercase();
                if name.ends_with(".mw.html") || name.ends_with(".mw.htm") {
                    ContentFormat::MediaWikiHtml(None)
                } else {
                    ContentFormat::WebHtml
                }
            }
            _ => bail!(
                "Choose .md, .markdown, .xhtml (flow XHTML), .html, .mw.html (Parsoid), or .epub"
            ),
        };
        Self::from_content(name, bytes, generation, format, None, None)
    }

    /// HTTP response media type takes precedence over a misleading URL suffix.
    pub fn from_response(
        url: &str,
        content_type: &str,
        bytes: Vec<u8>,
        generation: u64,
    ) -> Result<Self> {
        let url = external_url(url)?;
        let mut format = ContentFormat::for_content_type(content_type)?;
        if format == ContentFormat::WebHtml && url_request(url.as_str())?.wikipedia {
            // A host-routed Wikipedia response must be Parsoid, even if its
            // profile header is absent. Version metadata is still required.
            format = ContentFormat::MediaWikiHtml(None);
        }
        Self::from_content(
            url.as_str(),
            bytes,
            generation,
            format,
            Some(url.clone()),
            Some(content_type),
        )
    }

    fn from_content(
        name: &str,
        bytes: Vec<u8>,
        generation: u64,
        format: ContentFormat,
        source_url: Option<Url>,
        content_type: Option<&str>,
    ) -> Result<Self> {
        let _span = crate::telemetry::phase_span!(
            "flow.reader.import",
            generation = generation,
            format = format!("{format:?}"),
            bytes = bytes.len() as u64
        );
        ensure!(
            bytes.len() <= MAX_INPUT,
            "Input exceeds the 64 MiB demo limit"
        );
        let key = PublicationKey::new(format!("demo-open-{generation}"))?;
        if format == ContentFormat::Epub {
            let input = EpubPublication::from_bytes(bytes)?;
            let model = input.publication(key)?;
            ensure!(
                model.start().is_some(),
                "EPUB has no sequential document to open"
            );
            return Ok(Self {
                title: name.into(),
                model,
                input: Input::Epub(input),
                #[cfg(not(target_arch = "wasm32"))]
                encoded_resources: Default::default(),
            });
        }
        ensure!(
            bytes.len() <= MAX_DOCUMENT,
            "Standalone document exceeds the 16 MiB demo limit"
        );
        let text = {
            let _span = crate::telemetry::phase_span!(
                "flow.reader.decode_text",
                bytes = bytes.len() as u64
            );
            decode_text(&bytes, content_type)?
        };
        let resource_base = source_url.as_ref().map(ToString::to_string);
        let identity = resource_base.clone().map(DocumentReference::Host);
        let mut title = name.to_owned();
        let document = match format {
            ContentFormat::Markdown => {
                markdown::parse(
                    &text,
                    markdown::MarkdownOptions {
                        resource_base,
                        identity,
                        ..Default::default()
                    },
                )?
                .document
            }
            ContentFormat::FlowXhtml => {
                flow_xhtml::import(
                    &text,
                    flow_xhtml::XhtmlOptions {
                        canonical_version: flow_xhtml::CURRENT_FLOW_SEMANTIC_XHTML_VERSION,
                        resource_base,
                        identity,
                        ..Default::default()
                    },
                )?
                .document
            }
            ContentFormat::MediaWikiHtml(expected_version) => {
                let imported = mediawiki_html::import(
                    &text,
                    mediawiki_html::MediaWikiOptions {
                        source_url: resource_base,
                        identity,
                        expected_version,
                        ..Default::default()
                    },
                )?;
                // Database title is a window/document label, not a synthesized
                // display heading. Other wiki metadata has no UI in this demo.
                if let Some(database_title) = imported.metadata.database_title {
                    title = database_title;
                }
                imported.content.document
            }
            ContentFormat::WebHtml => {
                let imported = web_html::import(
                    &text,
                    web_html::WebOptions {
                        source_url: resource_base,
                        identity,
                        ..Default::default()
                    },
                )?;
                if !imported.metadata.title.is_empty() {
                    title = imported.metadata.title;
                }
                imported.content.document
            }
            ContentFormat::Epub => unreachable!(),
        };
        let document_key = DocumentKey::new("document")?;
        let start = source_url
            .as_ref()
            .and_then(Url::fragment)
            .map(|fragment| percent_encoding::percent_decode_str(fragment).decode_utf8())
            .transpose()?
            .map(|fragment| {
                flow_publication::document::model::flow::AnchorId::new(fragment.into_owned())
            })
            .filter(|anchor| document.anchor(anchor).is_some())
            .map(|anchor| PublicationTarget {
                document: document_key.clone(),
                anchor: Some(anchor),
            });
        let model = Publication::new(PublicationDescription {
            key,
            revision: PublicationRevision::new(format!("resident-{generation}"))?,
            metadata: PublicationMetadata {
                titles: vec![title.clone()],
                ..Default::default()
            },
            documents: vec![DocumentDescriptor {
                key: document_key.clone(),
                title: title.clone(),
                fonts: Vec::new(),
            }],
            reading_order: vec![document_key],
            navigation: Vec::new(),
            resources: Vec::new(),
            start,
        })?;
        Ok(Self {
            title,
            model,
            #[cfg(not(target_arch = "wasm32"))]
            encoded_resources: Default::default(),
            input: Input::Single {
                document: ImportedDocument::new(document),
                web_url: source_url,
                #[cfg(not(target_arch = "wasm32"))]
                path: None,
            },
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_url(url: &str, generation: u64) -> Result<Self> {
        let _span = crate::telemetry::phase_span!("flow.reader.open_url", generation = generation);
        let mut plan = url_request(url)?;
        let mut response = fetch_http(&plan.request_url, MAX_INPUT)
            .context("Could not fetch document (Wikipedia articles request Parsoid HTML)")?;
        if !plan.wikipedia {
            // Recognize a Wikipedia article reached through an HTTP redirect.
            let redirected = url_request(&response.url)?;
            if redirected.wikipedia {
                response = fetch_http(&redirected.request_url, MAX_INPUT)
                    .context("Could not fetch redirected Wikipedia article as Parsoid HTML")?;
                plan = redirected;
            }
        }
        let source = if plan.wikipedia {
            &plan.source_url
        } else {
            &response.url
        };
        Self::from_response(source, &response.content_type, response.bytes, generation)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_path(path: &std::path::Path, generation: u64) -> Result<Self> {
        let canonical = path
            .canonicalize()
            .context("Cannot resolve the selected file")?;
        let name = path
            .file_name()
            .context("File has no name")?
            .to_string_lossy();
        let mut book = Self::from_bytes(&name, read_bounded(&canonical, MAX_INPUT)?, generation)?;
        if let Input::Single { path, .. } = &mut book.input {
            *path = Some(canonical);
        }
        Ok(book)
    }

    fn check_scope(&self, key: &PublicationKey, revision: &PublicationRevision) -> Result<()> {
        ensure!(
            key == self.model.key() && revision == self.model.revision(),
            "Request belongs to an obsolete open document"
        );
        Ok(())
    }

    pub fn document(&self, request: &DocumentRequest) -> Result<ImportedDocument> {
        self.check_scope(&request.publication, &request.revision)?;
        ensure!(
            self.model.document(&request.document).is_some(),
            "Unknown document"
        );
        match &self.input {
            Input::Epub(input) => {
                Ok(input.import_document(&request.document, SourceRetention::Discard)?)
            }
            Input::Single { document, .. } => Ok(document.clone()),
        }
    }

    pub fn resource(&self, request: &ResourceRequest) -> Result<PreparedResource> {
        let _span = crate::telemetry::phase_span!(
            "flow.reader.resource",
            kind = format!("{:?}", request.kind)
        );
        self.check_scope(&request.publication, &request.revision)?;
        let bytes: Arc<[u8]> = match (&self.input, &request.resource) {
            (Input::Epub(input), ResourceReference::Publication(key)) => {
                input.resource_by_key(key)?.into()
            }
            (_, ResourceReference::Host(reference)) if reference.starts_with("data:") => {
                decode_data_image(reference)?.into()
            }
            #[cfg(not(target_arch = "wasm32"))]
            (
                Input::Single {
                    web_url: Some(base),
                    ..
                },
                ResourceReference::Host(reference),
            ) => {
                let url = base.join(reference)?;
                let key = url.to_string();
                let cached = self.encoded_resources.borrow().get(&key);
                if let Some(bytes) = cached {
                    bytes
                } else {
                    let bytes: Arc<[u8]> = fetch_http(&key, MAX_RESOURCE)?.bytes.into();
                    self.encoded_resources
                        .borrow_mut()
                        .insert(key, Arc::clone(&bytes));
                    bytes
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            (
                Input::Single {
                    path: Some(path), ..
                },
                ResourceReference::Host(reference),
            ) => read_bounded(&adjacent_path(path, reference)?, MAX_RESOURCE)?.into(),
            _ => bail!(
                "Resource unavailable: remote images require the desktop host; browser uploads cannot read neighboring files"
            ),
        };
        ensure!(
            bytes.len() <= MAX_RESOURCE,
            "Resource exceeds the 16 MiB demo limit"
        );
        match request.kind {
            ResourceKind::Font => Ok(PreparedResource::Font(FontResource::from_bytes(bytes))),
            ResourceKind::ImageMetadata => {
                let reader = image_reader(&bytes)?;
                let (width, height) = reader.into_dimensions()?;
                ensure!(
                    width <= 4096
                        && height <= 4096
                        && u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS,
                    "Image exceeds the 16 megapixel demo limit"
                );
                Ok(PreparedResource::ImageMetadata { width, height })
            }
            ResourceKind::ImagePixels {
                desired_width,
                desired_height,
            } => {
                let (width, height) = desired_width
                    .zip(desired_height)
                    .context("Missing decoded image dimensions")?;
                // Keep the response within the engine\'s decoded-variant budget.
                ensure!(
                    width != 0
                        && height != 0
                        && u64::from(width) * u64::from(height) <= 4 * 1024 * 1024,
                    "Displayed image exceeds the 4 megapixel demo limit"
                );
                let image = image_reader(&bytes)?.decode()?;
                let pixels = if image.width() == width && image.height() == height {
                    image.to_rgba8()
                } else {
                    image
                        .resize_exact(width, height, image::imageops::FilterType::Triangle)
                        .to_rgba8()
                };
                Ok(PreparedResource::ImagePixels(ImagePixels::from(pixels)))
            }
        }
    }

    pub fn resolve_link(&self, destination: Destination) -> Result<ResolvedLink> {
        match destination {
            Destination::External(reference) => {
                external_url(&reference)?;
                Ok(ResolvedLink::External(reference))
            }
            Destination::Local(anchor) => Ok(self.self_link(Some(anchor))),
            Destination::Document { document, anchor } => {
                let reference = match document {
                    DocumentReference::Relative(value)
                    | DocumentReference::Host(value)
                    | DocumentReference::PublicationItem(value) => value,
                };
                if let Ok(url) = Url::parse(&reference)
                    && matches!(url.scheme(), "http" | "https")
                {
                    let mut url = url;
                    url.set_fragment(anchor.as_ref().map(|anchor| anchor.as_str()));
                    return Ok(ResolvedLink::External(url.into()));
                }
                if let Input::Single {
                    web_url: Some(base),
                    ..
                } = &self.input
                {
                    let mut url = base.join(&reference)?;
                    url.set_fragment(anchor.as_ref().map(|anchor| anchor.as_str()));
                    external_url(url.as_str())?;
                    return Ok(ResolvedLink::External(url.into()));
                }
                #[cfg(not(target_arch = "wasm32"))]
                if let Input::Single {
                    path: Some(path), ..
                } = &self.input
                    && adjacent_path(path, &reference).is_ok_and(|target| &target == path)
                {
                    return Ok(self.self_link(anchor));
                }
                bail!(
                    "Only links inside the current document/EPUB and HTTP(S) external links are supported"
                )
            }
        }
    }

    fn self_link(
        &self,
        anchor: Option<flow_publication::document::model::flow::AnchorId>,
    ) -> ResolvedLink {
        ResolvedLink::Target(NavigationTarget {
            publication: self.model.key().clone(),
            revision: Some(self.model.revision().clone()),
            target: PublicationTarget {
                document: self
                    .model
                    .start()
                    .expect("openable model has a start")
                    .document,
                anchor,
            },
        })
    }
}

pub fn external_url(reference: &str) -> Result<Url> {
    let url = Url::parse(reference)?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "Only HTTP(S) links can leave the demo"
    );
    Ok(url)
}

fn image_reader(bytes: &[u8]) -> Result<image::ImageReader<Cursor<&[u8]>>> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader)
}

fn decode_data_image(reference: &str) -> Result<Vec<u8>> {
    ensure!(
        reference.len() <= MAX_RESOURCE * 2,
        "Data image exceeds demo limit"
    );
    let (header, payload) = reference.split_once(',').context("Invalid data image")?;
    ensure!(
        header.starts_with("data:image/") && header.ends_with(";base64"),
        "Only base64 data images are supported"
    );
    Ok(base64::engine::general_purpose::STANDARD.decode(payload)?)
}

#[cfg(not(target_arch = "wasm32"))]
fn read_bounded(path: &std::path::Path, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    ensure!(std::fs::metadata(path)?.is_file(), "Not a regular file");
    let file = std::fs::File::open(path)?;
    ensure!(file.metadata()?.is_file(), "Not a regular file");
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "File exceeds the demo byte limit");
    Ok(bytes)
}

#[cfg(not(target_arch = "wasm32"))]
fn adjacent_path(document: &std::path::Path, reference: &str) -> Result<std::path::PathBuf> {
    let root = document.parent().context("Document has no directory")?;
    let base = Url::from_directory_path(root)
        .map_err(|_| anyhow::anyhow!("Invalid document directory"))?;
    let mut url = base.join(reference)?;
    ensure!(
        url.scheme() == "file" && url.query().is_none(),
        "Remote resources are disabled"
    );
    url.set_fragment(None);
    ensure!(
        url.host_str() == base.host_str(),
        "Resource changes the selected file authority"
    );
    let candidate = url
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("Invalid local resource"))?;
    // File URLs normalize Windows verbatim prefixes to ordinary drive/UNC paths.
    // Compare both lexical paths in the URL representation, then canonical
    // filesystem paths below so symlinks cannot escape the selected directory.
    let lexical_root = base
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("Invalid local directory"))?;
    ensure!(
        candidate.starts_with(&lexical_root),
        "Resource escapes the selected document directory"
    );
    let path = candidate.canonicalize()?;
    ensure!(
        path.starts_with(root),
        "Resource escapes the selected document's directory"
    );
    Ok(path)
}

fn decode_text(bytes: &[u8], content_type: Option<&str>) -> Result<String> {
    let charset = content_type.and_then(|value| {
        value.split(';').skip(1).find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches(['\"', '\'']))
        })
    });
    let encoding = if let Some(label) = charset {
        encoding_rs::Encoding::for_label(label.as_bytes())
            .context("Unsupported response charset")?
    } else {
        encoding_rs::UTF_8
    };
    let (text, _, errors) = encoding.decode(bytes);
    ensure!(
        !errors,
        "Document is not valid in its declared encoding (default UTF-8)"
    );
    Ok(text.into_owned())
}

#[cfg(not(target_arch = "wasm32"))]
struct HttpInput {
    url: String,
    content_type: String,
    bytes: Vec<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_http(reference: &str, limit: usize) -> Result<HttpInput> {
    use std::io::Read;
    let url = external_url(reference)?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Credentials in URLs are not supported"
    );
    let _span = crate::telemetry::phase_span!(
        "flow.reader.http",
        host = url.host_str().unwrap_or_default(),
        limit_bytes = limit as u64
    );
    let client = http_client()?;
    let response = {
        let _span = crate::telemetry::phase_span!("flow.reader.http_headers");
        let response = client.get(url).send()?;
        crate::telemetry::phase_event!(
            "flow.reader.http_status",
            milestone = "http_status",
            status = response.status().as_u16() as u64
        );
        response.error_for_status()?
    };
    let url = response.url().to_string();
    external_url(&url)?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .context("Response has no valid Content-Type")?
        .to_owned();
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= limit as u64),
        "Response exceeds the demo byte limit"
    );
    let mut bytes = Vec::new();
    {
        let _span = crate::telemetry::phase_span!("flow.reader.http_body");
        response.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    }
    crate::telemetry::phase_event!(
        "flow.reader.http_complete",
        milestone = "http_complete",
        bytes = bytes.len() as u64
    );
    ensure!(bytes.len() <= limit, "Response exceeds the demo byte limit");
    Ok(HttpInput {
        url,
        content_type,
        bytes,
    })
}

#[cfg(test)]
mod tests;
