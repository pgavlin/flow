//! Host-owned input, adapter selection, resource policy and decoding.
use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use flow_publication::{
    document::{
        formats::{flow_xhtml, markdown},
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

pub const MAX_INPUT: usize = 64 * 1024 * 1024;
const MAX_DOCUMENT: usize = 16 * 1024 * 1024;
const MAX_RESOURCE: usize = 16 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;

pub const WELCOME: &str = "# flow demo\n\nOpen a Markdown document, flow semantic XHTML, or EPUB.\n\n- **Ctrl/Cmd+O**: open (or drop a file on the desktop window)\n- **P**: toggle continuous / paginated reading\n- **Wheel, Space, Page Up/Down**: scroll or turn pages\n- **Left/Right**: previous/next page (or EPUB document in continuous mode)\n- **Alt+Left** or **Backspace**: back\n- **Ctrl/Cmd + / −**: font size\n\nClick document links to follow them. This demo has no library, persistence, remote resource fetching, or settings screen.\n\nIn a browser, use the Open button. EPUB images and fonts are self-contained; an uploaded standalone file cannot read neighboring files on your disk.";

pub struct Book {
    pub title: String,
    pub model: Publication,
    input: Input,
}
enum Input {
    Epub(EpubPublication),
    Single {
        document: ImportedDocument,
        #[cfg(not(target_arch = "wasm32"))]
        path: Option<std::path::PathBuf>,
    },
}

impl Book {
    pub fn from_bytes(name: &str, bytes: Vec<u8>, generation: u64) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_INPUT,
            "File exceeds the 64 MiB demo limit"
        );
        let key = PublicationKey::new(format!("demo-open-{generation}"))?;
        let extension = name
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase());
        if extension.as_deref() == Some("epub") {
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
            });
        }
        ensure!(
            bytes.len() <= MAX_DOCUMENT,
            "Standalone document exceeds the 16 MiB demo limit"
        );
        let text = std::str::from_utf8(&bytes).context("Standalone documents must be UTF-8")?;
        let document = match extension.as_deref() {
            Some("md" | "markdown") => markdown::parse(text, Default::default())?.document,
            Some("xhtml" | "html") => {
                flow_xhtml::import(
                    text,
                    flow_xhtml::XhtmlOptions {
                        canonical_version: flow_xhtml::CURRENT_FLOW_SEMANTIC_XHTML_VERSION,
                        ..Default::default()
                    },
                )?
                .document
            }
            _ => bail!("Choose .md, .markdown, .xhtml, .html (flow XHTML), or .epub"),
        };
        let document_key = DocumentKey::new("document")?;
        let model = Publication::new(PublicationDescription {
            key,
            revision: PublicationRevision::new(format!("resident-{generation}"))?,
            metadata: PublicationMetadata {
                titles: vec![name.into()],
                ..Default::default()
            },
            documents: vec![DocumentDescriptor {
                key: document_key.clone(),
                title: name.into(),
                fonts: Vec::new(),
            }],
            reading_order: vec![document_key],
            navigation: Vec::new(),
            resources: Vec::new(),
            start: None,
        })?;
        Ok(Self {
            title: name.into(),
            model,
            input: Input::Single {
                document: ImportedDocument::new(document),
                #[cfg(not(target_arch = "wasm32"))]
                path: None,
            },
        })
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
        self.check_scope(&request.publication, &request.revision)?;
        let bytes = match (&self.input, &request.resource) {
            (Input::Epub(input), ResourceReference::Publication(key)) => {
                input.resource_by_key(key)?
            }
            (_, ResourceReference::Host(reference)) if reference.starts_with("data:") => {
                decode_data_image(reference)?
            }
            #[cfg(not(target_arch = "wasm32"))]
            (
                Input::Single {
                    path: Some(path), ..
                },
                ResourceReference::Host(reference),
            ) => read_bounded(&adjacent_path(path, reference)?, MAX_RESOURCE)?,
            _ => bail!(
                "Resource unavailable: no remote fetching or browser access to neighboring files"
            ),
        };
        ensure!(
            bytes.len() <= MAX_RESOURCE,
            "Resource exceeds the 16 MiB demo limit"
        );
        match request.kind {
            ResourceKind::Font => Ok(PreparedResource::Font(FontResource::from_bytes(Arc::from(
                bytes,
            )))),
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

#[cfg(test)]
mod tests;
