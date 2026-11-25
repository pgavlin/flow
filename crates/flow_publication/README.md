# flow_publication

The format-neutral publication layer of [flow](https://github.com/pgavlin/flow),
built on `flow_document`.

An immutable publication describes documents, a de-duplicated reading order,
navigation and resources. `ReadingSession` coordinates navigation, history,
bounded residency, resource work, pagination and durable reading locations.
Hosts acquire content, select adapters and fulfill explicit requests. The engine
never opens a path or fetches a URL.

The optional `formats::epub` adapter translates EPUB archive bytes into the
publication model and imports its documents.

```rust
use flow_publication::publication::{
    DocumentDescriptor, DocumentKey, Publication, PublicationDescription,
    PublicationKey, PublicationMetadata, PublicationRevision,
};
let document = DocumentKey::new("chapter")?;
let model = Publication::new(PublicationDescription {
    key: PublicationKey::new("host book identity")?,
    revision: PublicationRevision::new("edition one")?,
    metadata: PublicationMetadata::default(),
    documents: vec![DocumentDescriptor {
        key: document.clone(), title: "Chapter".into(), fonts: Vec::new(),
    }],
    reading_order: vec![document.clone()],
    navigation: Vec::new(), resources: Vec::new(), start: None,
})?;

#[cfg(feature = "render")]
{
    use flow_publication::{
        document::{formats::markdown, settings::Settings},
        publication::{ImportedDocument, PublicationTarget},
        reading_session::{ReadingSession, SessionEffect},
    };
    let mut session = ReadingSession::new(Settings::default());
    let handle = session.register_publication(model)?;
    session.open_target(handle, PublicationTarget::document(document))?;
    for effect in session.take_effects()? {
        if let SessionEffect::LoadDocument(request) = effect {
            // This host materializes from memory; another can fetch asynchronously.
            let imported = markdown::parse("A synthetic chapter.", Default::default())?;
            session.provide_document(request.id, ImportedDocument::new(imported.document))?;
        }
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Document APIs live in the `flow_publication::document` namespace
(or directly in `flow_document`), not flattened into the publication crate.

```compile_fail
use flow_publication::viewer::DocumentViewer;
```

## Features

Defaults are empty: the generic model, registry, outline, location data and
lightweight publication codecs need neither rendering nor ZIP. Features add
consumer-facing capabilities:

- `serde`: model/location serialization, including core text-position evidence.
- `epub`: `EpubPublication::from_bytes`, model translation, document import and
  bounded resource access. Does not acquire archive bytes.
- `render`: `ReadingSession`, document rendering and layout-artifact support;
  does not enable `epub` or publication Serde derives.
- `mediawiki-html`: enables the independent expanded Parsoid DOM importer under
  `document::formats::mediawiki_html`; no wiki fetching or publication adapter.
- `telemetry`: tracing instrumentation using the host's subscriber.

Hosts store cache bytes opaquely and supply exact dependency digests to the
owning codecs. Saved locations use schema 2: publication/document identity plus
canonical-text evidence. Hosts choose their serialization format.
