use super::registry::{PublicationRegistry, RegistryError};
use super::*;

pub(crate) fn description() -> PublicationDescription {
    PublicationDescription {
        key: PublicationKey::new("opaque identity: not/a/URL").unwrap(),
        revision: PublicationRevision::new("edition α").unwrap(),
        metadata: PublicationMetadata::default(),
        documents: ["first", "second", "auxiliary"]
            .into_iter()
            .map(|key| DocumentDescriptor {
                key: DocumentKey::new(key).unwrap(),
                title: key.into(),
                fonts: Vec::new(),
            })
            .collect(),
        reading_order: ["first", "first", "second", "first"]
            .into_iter()
            .map(|key| DocumentKey::new(key).unwrap())
            .collect(),
        navigation: Vec::new(),
        resources: Vec::new(),
        start: None,
    }
}

#[test]
fn model_deduplicates_reading_order_and_keeps_auxiliary_documents() {
    let model = Publication::new(description()).unwrap();
    assert_eq!(
        model
            .reading_order()
            .iter()
            .map(DocumentKey::as_str)
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(
        model
            .document(&DocumentKey::new("auxiliary").unwrap())
            .is_some()
    );
    assert_eq!(model.start().unwrap().document.as_str(), "first");
}

#[test]
fn validation_rejects_conflicts_unknown_targets_and_unbounded_input() {
    let mut input = description();
    input.documents.push(input.documents[0].clone());
    assert_eq!(
        Publication::new(input).unwrap_err(),
        ModelError::DuplicateKey
    );
    let mut input = description();
    input
        .reading_order
        .push(DocumentKey::new("absent").unwrap());
    assert!(matches!(
        Publication::new(input),
        Err(ModelError::UnknownDocument(_))
    ));
    let mut input = description();
    input.documents[0]
        .fonts
        .push(ResourceKey::new("absent").unwrap());
    assert!(matches!(
        Publication::new(input),
        Err(ModelError::UnknownResource(_))
    ));
    assert!(PublicationKey::new("").is_err());
    assert!(DocumentKey::new("\0").is_err());
    assert!(PublicationRevision::new("x".repeat(MAX_KEY_BYTES + 1)).is_err());
    let mut input = description();
    input.reading_order.clear();
    assert!(Publication::new(input).unwrap().start().is_none());
}

#[test]
fn registry_uses_host_identity_not_content_aliases_or_schemes() {
    let mut registry = PublicationRegistry::new();
    let original = Publication::new(description()).unwrap();
    let first = registry.register(original.clone()).unwrap();
    assert_eq!(registry.register(original).unwrap(), first);
    let mut alias = description();
    alias.key = PublicationKey::new("file:///book.epub").unwrap();
    assert_ne!(
        registry.register(Publication::new(alias).unwrap()).unwrap(),
        first
    );
    let mut conflict = description();
    conflict.metadata.titles.push("changed".into());
    assert_eq!(
        registry
            .register(Publication::new(conflict).unwrap())
            .unwrap_err(),
        RegistryError::RevisionConflict
    );
    let mut newer = description();
    newer.revision = PublicationRevision::new("newer").unwrap();
    let next = registry.register(Publication::new(newer).unwrap()).unwrap();
    assert_ne!(first, next);
    assert_eq!(registry.latest(&description().key), Some(next));
    assert_eq!(
        registry.revision(&description().key, &description().revision),
        Some(first)
    );
    let mut another = PublicationRegistry::new();
    let other = another
        .register(Publication::new(description()).unwrap())
        .unwrap();
    assert_ne!(first, other);
    assert!(another.get(first).is_none());
}

#[cfg(feature = "serde")]
#[test]
fn serialized_descriptions_and_locations_are_core_data() {
    use crate::location::*;
    use flow_document::{
        formats::markdown,
        position::{StableTextPosition, TextFingerprint},
    };
    let document = markdown::parse("hello café", Default::default())
        .unwrap()
        .document;
    let input = description();
    let location = PersistentReadingLocation {
        version: PERSISTENT_LOCATION_VERSION,
        publication: input.key.clone(),
        publication_revision: input.revision.clone(),
        document: input.documents[0].key.clone(),
        text_fingerprint: TextFingerprint::of_document(&document),
        position: StableTextPosition::capture(document.text().as_str(), 3),
        anchor_hint: None,
    };
    let json = serde_json::to_string(&location).unwrap();
    let decoded: PersistentReadingLocation = serde_json::from_str(&json).unwrap();
    decoded.validate().unwrap();
    assert_eq!(location, decoded);
    assert_eq!(
        Publication::new(serde_json::from_str(&serde_json::to_string(&input).unwrap()).unwrap())
            .unwrap(),
        Publication::new(input).unwrap()
    );
    assert!(serde_json::from_str::<PublicationKey>("\"\"").is_err());
}
