//! Translation of EPUB package namespaces into neutral publication data.
use super::*;
use crate::publication::{
    DocumentDescriptor, DocumentKey, ImportedDocument, LinkBinding, Publication,
    PublicationDescription, PublicationKey, PublicationRevision, PublicationTarget,
    ResourceDescriptor, ResourceKey, ResourceReference,
};
use crate::toc::PublicationNavigationEntry;
use flow_document::model::flow::{Destination, NodeKind};

impl EpubPublication {
    /// The host supplies identity; OPF identifiers are metadata, not routing keys.
    pub fn publication(&self, key: PublicationKey) -> Result<Publication, EpubError> {
        let fonts = self
            .manifest
            .values()
            .filter(|item| is_font(&item.media_type))
            .map(|item| ResourceKey::new(&item.id))
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = BTreeSet::new();
        let documents = self
            .manifest
            .values()
            .filter(|item| {
                item.media_type == "application/xhtml+xml"
                    || item.media_type == "text/html"
                    || self.spine.iter().any(|entry| entry.href == item.href)
            })
            .filter(|item| seen.insert(item.href.clone()))
            .map(|item| {
                Ok(DocumentDescriptor {
                    key: self
                        .document_key(&item.href)
                        .ok_or_else(|| EpubError::UnknownItem(item.id.clone()))?,
                    title: item.href.clone(),
                    fonts: fonts.clone(),
                })
            })
            .collect::<Result<Vec<_>, EpubError>>()?;
        let reading_order = self
            .spine
            .iter()
            .filter(|item| item.linear)
            .map(|item| {
                self.document_key(&item.href)
                    .ok_or_else(|| EpubError::UnknownItem(item.item_id.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let resources = self
            .manifest
            .values()
            .map(|item| {
                Ok(ResourceDescriptor {
                    key: ResourceKey::new(&item.id)?,
                    media_type: Some(item.media_type.clone()),
                })
            })
            .collect::<Result<Vec<_>, EpubError>>()?;
        Ok(Publication::new(PublicationDescription {
            key,
            revision: PublicationRevision::new(&self.revision)?,
            metadata: crate::publication::PublicationMetadata {
                titles: self.metadata.titles.clone(),
                creators: self.metadata.creators.clone(),
                languages: self.metadata.languages.clone(),
            },
            documents,
            reading_order,
            navigation: self
                .navigation
                .iter()
                .map(|point| self.neutral_navigation(point))
                .collect::<Result<_, _>>()?,
            resources,
            start: None,
        })?)
    }

    /// Maps a manifest ID or normalized package href into the adapter's stable
    /// document namespace. Aliases of one package member identify one document.
    pub fn document_key(&self, identity: &str) -> Option<DocumentKey> {
        let href = self
            .manifest
            .get(identity)
            .map_or(identity, |item| item.href.as_str());
        let id = self.manifest_by_href.get(href)?;
        let item = self.manifest.get(id)?;
        if item.media_type != "application/xhtml+xml"
            && item.media_type != "text/html"
            && !self.spine.iter().any(|entry| entry.href == href)
        {
            return None;
        }
        DocumentKey::new(id).ok()
    }

    /// Materializes any declared flow-capable document, including navigation-only
    /// documents outside the spine. Never performs host acquisition.
    pub fn import_document(
        &self,
        key: &DocumentKey,
        retention: SourceRetention,
    ) -> Result<ImportedDocument, EpubError> {
        let _span = flow_document::telemetry::phase_span!(
            "flow.publication.document.import",
            document = key.as_str()
        );
        let item = self
            .manifest
            .get(key.as_str())
            .ok_or_else(|| EpubError::UnknownItem(key.to_string()))?;
        let mut result = ImportedDocument::new(self.import_item(item, retention)?.document);
        for id in result.document.preorder(result.document.root()) {
            let node = result
                .document
                .get_node(id)
                .expect("preorder yields valid nodes");
            let destination = match &node.kind {
                NodeKind::Link(link) => Some(&link.destination),
                NodeKind::UnsupportedObject(object) => object.activation.as_ref(),
                _ => None,
            };
            if let Some(Destination::Document { document, .. }) = destination {
                let href = match document {
                    DocumentReference::Relative(path) => resolve_href(&item.href, path, true).ok(),
                    DocumentReference::PublicationItem(identity) => Some(identity.clone()),
                    DocumentReference::Host(_) => None,
                };
                if let Some(key) = href.and_then(|href| self.document_key(&href)) {
                    result.links.insert(
                        document.clone(),
                        LinkBinding::Document(PublicationTarget::document(key)),
                    );
                }
            }
        }
        for (_, resource) in result.document.resources() {
            let binding =
                if has_uri_scheme(&resource.identity) || resource.identity.starts_with("//") {
                    ResourceReference::Host(resource.identity.clone())
                } else {
                    let href = resolve_href(&item.href, &resource.identity, true)?;
                    let id = self
                        .manifest_by_href
                        .get(&href)
                        .ok_or_else(|| EpubError::UndeclaredResource(href.clone()))?;
                    ResourceReference::Publication(ResourceKey::new(id)?)
                };
            result.resources.insert(resource.identity.clone(), binding);
        }
        Ok(result)
    }

    pub fn resource_by_key(&self, key: &ResourceKey) -> Result<Vec<u8>, EpubError> {
        self.resource(key.as_str())
    }

    fn neutral_navigation(
        &self,
        point: &NavigationPoint,
    ) -> Result<PublicationNavigationEntry, EpubError> {
        Ok(PublicationNavigationEntry {
            label: point.label.clone(),
            target: PublicationTarget {
                document: self
                    .document_key(&point.href)
                    .ok_or_else(|| EpubError::UnknownItem(point.href.clone()))?,
                anchor: point
                    .fragment
                    .as_ref()
                    .map(flow_document::model::flow::AnchorId::new),
            },
            children: point
                .children
                .iter()
                .map(|child| self.neutral_navigation(child))
                .collect::<Result<_, _>>()?,
        })
    }
}

fn is_font(media_type: &str) -> bool {
    media_type.starts_with("font/")
        || matches!(
            media_type,
            "application/vnd.ms-opentype" | "application/font-sfnt" | "application/x-font-ttf"
        )
}
