//! Immutable, format-neutral publication descriptions and semantic bindings.

use std::collections::{HashMap, HashSet};
use std::fmt;

use flow_document::model::flow::{AnchorId, DocumentReference, FlowDocument};
use thiserror::Error;

pub mod registry;

const MAX_KEY_BYTES: usize = 4096;
const MAX_ITEMS: usize = 100_000;
const MAX_NAVIGATION_DEPTH: usize = 64;

macro_rules! key {
    ($name:ident) => {
        #[doc = "Opaque, bounded identity; never interpreted as a path or URL."]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        #[cfg_attr(feature = "serde", serde(try_from = "String", into = "String"))]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
                let value = value.into();
                if value.is_empty() || value.len() > MAX_KEY_BYTES || value.contains('\0') {
                    return Err(ModelError::InvalidKey);
                }
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = ModelError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}
key!(PublicationKey);
key!(PublicationRevision);
key!(DocumentKey);
key!(ResourceKey);

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PublicationMetadata {
    pub titles: Vec<String>,
    pub creators: Vec<String>,
    pub languages: Vec<String>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentDescriptor {
    pub key: DocumentKey,
    pub title: String,
    /// Fonts required for stable layout, in deterministic registration order.
    pub fonts: Vec<ResourceKey>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceDescriptor {
    pub key: ResourceKey,
    pub media_type: Option<String>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PublicationTarget {
    pub document: DocumentKey,
    pub anchor: Option<AnchorId>,
}

impl PublicationTarget {
    pub fn document(document: DocumentKey) -> Self {
        Self {
            document,
            anchor: None,
        }
    }
}

/// Unvalidated input. Use `Publication::new` before registering it.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationDescription {
    pub key: PublicationKey,
    pub revision: PublicationRevision,
    pub metadata: PublicationMetadata,
    pub documents: Vec<DocumentDescriptor>,
    /// Repeated references are normalized to their first occurrence.
    /// Auxiliary/non-sequential documents belong only in the catalog.
    pub reading_order: Vec<DocumentKey>,
    pub navigation: Vec<crate::toc::PublicationNavigationEntry>,
    pub resources: Vec<ResourceDescriptor>,
    pub start: Option<PublicationTarget>,
}

/// A validated immutable description; contains no source loader or adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Publication {
    description: PublicationDescription,
    documents: HashMap<DocumentKey, usize>,
    resources: HashMap<ResourceKey, usize>,
    order: HashMap<DocumentKey, usize>,
}

impl Publication {
    pub fn new(mut description: PublicationDescription) -> Result<Self, ModelError> {
        if description.documents.len() > MAX_ITEMS
            || description.resources.len() > MAX_ITEMS
            || description.reading_order.len() > MAX_ITEMS
        {
            return Err(ModelError::Limit);
        }
        let mut documents = HashSet::new();
        for doc in &description.documents {
            if !documents.insert(&doc.key) {
                return Err(ModelError::DuplicateKey);
            }
            check_string(&doc.title)?;
            if doc.fonts.len() > 256 {
                return Err(ModelError::Limit);
            }
        }
        let mut resources = HashSet::new();
        for resource in &description.resources {
            if !resources.insert(&resource.key) {
                return Err(ModelError::DuplicateKey);
            }
            if let Some(media_type) = &resource.media_type {
                check_string(media_type)?;
            }
        }
        for doc in &description.documents {
            for font in &doc.fonts {
                if !resources.contains(font) {
                    return Err(ModelError::UnknownResource(font.clone()));
                }
            }
        }
        for item in &description.reading_order {
            if !documents.contains(item) {
                return Err(ModelError::UnknownDocument(item.clone()));
            }
        }
        let mut seen = HashSet::new();
        description
            .reading_order
            .retain(|key| seen.insert(key.clone()));
        let check_target = |target: &PublicationTarget| {
            if !documents.contains(&target.document) {
                return Err(ModelError::UnknownDocument(target.document.clone()));
            }
            if let Some(anchor) = &target.anchor {
                check_string(anchor.as_str())?;
            }
            Ok(())
        };
        if let Some(start) = &description.start {
            check_target(start)?;
        }
        let mut stack = description
            .navigation
            .iter()
            .map(|node| (node, 1))
            .collect::<Vec<_>>();
        let mut count = 0;
        while let Some((node, depth)) = stack.pop() {
            count += 1;
            if count > MAX_ITEMS || depth > MAX_NAVIGATION_DEPTH {
                return Err(ModelError::Limit);
            }
            check_string(&node.label)?;
            check_target(&node.target)?;
            stack.extend(node.children.iter().map(|child| (child, depth + 1)));
            if stack.len() > MAX_ITEMS {
                return Err(ModelError::Limit);
            }
        }
        for values in [
            &description.metadata.titles,
            &description.metadata.creators,
            &description.metadata.languages,
        ] {
            if values.len() > MAX_ITEMS {
                return Err(ModelError::Limit);
            }
            for value in values {
                check_string(value)?;
            }
        }
        let documents = description
            .documents
            .iter()
            .enumerate()
            .map(|(index, doc)| (doc.key.clone(), index))
            .collect();
        let resources = description
            .resources
            .iter()
            .enumerate()
            .map(|(index, resource)| (resource.key.clone(), index))
            .collect();
        let order = description
            .reading_order
            .iter()
            .enumerate()
            .map(|(index, key)| (key.clone(), index))
            .collect();
        Ok(Self {
            description,
            documents,
            resources,
            order,
        })
    }

    pub fn key(&self) -> &PublicationKey {
        &self.description.key
    }
    pub fn revision(&self) -> &PublicationRevision {
        &self.description.revision
    }
    pub fn metadata(&self) -> &PublicationMetadata {
        &self.description.metadata
    }
    pub fn description(&self) -> &PublicationDescription {
        &self.description
    }
    pub fn documents(&self) -> &[DocumentDescriptor] {
        &self.description.documents
    }
    pub fn document(&self, key: &DocumentKey) -> Option<&DocumentDescriptor> {
        self.documents
            .get(key)
            .map(|index| &self.description.documents[*index])
    }
    pub fn resources(&self) -> &[ResourceDescriptor] {
        &self.description.resources
    }
    pub fn resource(&self, key: &ResourceKey) -> Option<&ResourceDescriptor> {
        self.resources
            .get(key)
            .map(|index| &self.description.resources[*index])
    }
    pub fn reading_order(&self) -> &[DocumentKey] {
        &self.description.reading_order
    }
    pub fn navigation(&self) -> &[crate::toc::PublicationNavigationEntry] {
        &self.description.navigation
    }
    pub fn start(&self) -> Option<PublicationTarget> {
        self.description.start.clone().or_else(|| {
            self.reading_order()
                .first()
                .cloned()
                .map(PublicationTarget::document)
        })
    }
    pub fn reading_order_index(&self, key: &DocumentKey) -> Option<usize> {
        self.order.get(key).copied()
    }
}

/// Resolution supplied by the adapter that understands the source namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkBinding {
    Document(PublicationTarget),
    Host(String),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ResourceReference {
    Publication(ResourceKey),
    Host(String),
}

/// Semantic input plus source-specific reference translations. Bindings are
/// checked against the requested publication before session installation.
#[derive(Clone, Debug)]
pub struct ImportedDocument {
    pub document: FlowDocument,
    pub links: HashMap<DocumentReference, LinkBinding>,
    pub resources: HashMap<String, ResourceReference>,
}

impl ImportedDocument {
    pub fn new(document: FlowDocument) -> Self {
        Self {
            document,
            links: HashMap::new(),
            resources: HashMap::new(),
        }
    }

    pub fn validate(&self, publication: &Publication) -> Result<(), ModelError> {
        if self.links.len() > MAX_ITEMS
            || self.resources.len() > MAX_ITEMS
            || self.document.resources().len() > MAX_ITEMS
        {
            return Err(ModelError::Limit);
        }
        for (source, target) in &self.links {
            let identity = match source {
                DocumentReference::Relative(value)
                | DocumentReference::PublicationItem(value)
                | DocumentReference::Host(value) => value,
            };
            check_string(identity)?;
            match target {
                LinkBinding::Document(target) => {
                    if publication.document(&target.document).is_none() {
                        return Err(ModelError::UnknownDocument(target.document.clone()));
                    }
                    if let Some(anchor) = &target.anchor {
                        check_string(anchor.as_str())?;
                    }
                }
                LinkBinding::Host(value) => check_string(value)?,
            }
        }
        for (source, resource) in &self.resources {
            check_string(source)?;
            match resource {
                ResourceReference::Publication(key) => {
                    if publication.resource(key).is_none() {
                        return Err(ModelError::UnknownResource(key.clone()));
                    }
                }
                ResourceReference::Host(value) => check_string(value)?,
            }
        }
        Ok(())
    }
}

pub(crate) fn check_string(value: &str) -> Result<(), ModelError> {
    if value.len() > MAX_KEY_BYTES || value.contains('\0') {
        Err(ModelError::Limit)
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ModelError {
    #[error("identity must be nonempty, bounded, and contain no NUL")]
    InvalidKey,
    #[error("publication input exceeds its structural limits")]
    Limit,
    #[error("duplicate catalog identity")]
    DuplicateKey,
    #[error("unknown document '{0}'")]
    UnknownDocument(DocumentKey),
    #[error("unknown resource '{0}'")]
    UnknownResource(ResourceKey),
}

#[cfg(test)]
mod tests;
