//! Incremental publication pagination over supplied semantic documents.
use super::*;
use flow_document::viewer::ViewerEffect;

pub(super) struct PaginationDocument {
    pub key: DocumentCacheKey,
    pub viewer: DocumentViewer,
    pub bindings: Arc<Bindings>,
    pub waiting_metadata: HashSet<String>,
    semantic_key: Option<SemanticKey>,
}

pub(super) struct PublicationPagination {
    pub publication: PublicationHandle,
    viewport: (u32, u32),
    pages: Vec<Option<u32>>,
    pub job: Option<PaginationDocument>,
    pub failure: Option<LoadFailure>,
}

impl ReadingSession {
    /// Starts/continues bounded work. True means an index exists, not that all
    /// its counts are known. Each host poll advances at most one document.
    pub fn prepare_publication_pagination(&mut self) -> Result<bool, ReadingSessionError> {
        let Some(active) = &self.active else {
            return Ok(false);
        };
        let publication = active.key.publication;
        let viewport = self.viewer.viewport_size();
        if self.settings.reading_mode != ReadingMode::Paginated
            || viewport.0 == 0
            || viewport.1 == 0
        {
            return Ok(false);
        }
        self.viewer.prepare_pagination(viewport.0, viewport.1);
        if self
            .publication_pagination
            .as_ref()
            .is_none_or(|index| index.publication != publication || index.viewport != viewport)
        {
            self.clear_pagination();
            let pages = vec![None; self.model(publication)?.reading_order().len()];
            self.publication_pagination = Some(PublicationPagination {
                publication,
                viewport,
                pages,
                job: None,
                failure: None,
            });
        }
        self.advance_pagination()?;
        Ok(true)
    }

    pub fn publication_page_index(&self) -> Option<&[Option<u32>]> {
        Some(&self.publication_pagination.as_ref()?.pages)
    }
    pub fn publication_pagination_failure(&self) -> Option<&LoadFailure> {
        self.publication_pagination.as_ref()?.failure.as_ref()
    }
    pub fn publication_pagination_complete(&self) -> bool {
        self.publication_pagination
            .as_ref()
            .is_some_and(|index| index.failure.is_none() && index.pages.iter().all(Option::is_some))
    }

    /// Returns a total only when every sequential document is stably paginated.
    pub fn publication_paginated_page_metrics(&self) -> Option<(usize, usize)> {
        if !self.publication_pagination_complete() {
            return None;
        }
        let active = self.active.as_ref()?;
        let index = self.publication_pagination.as_ref()?;
        if index.publication != active.key.publication {
            return None;
        }
        let current = self
            .publications
            .get(index.publication)?
            .reading_order_index(&active.key.document)?;
        let offset = index.pages[..current]
            .iter()
            .try_fold(0usize, |total, page| total.checked_add((*page)? as usize))?;
        let total = index
            .pages
            .iter()
            .try_fold(0usize, |total, page| total.checked_add((*page)? as usize))?;
        self.viewer.paginated_layout()?;
        let local = self.viewer.paginated_page_metrics()?.0;
        Some((offset.checked_add(local)?, total))
    }

    /// The caller supplies an exact dependency digest for all document import
    /// recipes and resolved layout resources, including external image metadata.
    /// A source URI or archive revision alone is not such a dependency digest.
    pub fn persistent_publication_index_digest(
        &self,
        content_dependency: crate::cache_codec::DependencyDigest,
    ) -> Option<[u8; 32]> {
        let active = self.active.as_ref()?;
        let model = self.publications.get(active.key.publication)?;
        // Fonts may differ per document. Do not key a whole-publication artifact
        // solely by the currently active renderer's font environment.
        let mut digest = blake3::Hasher::new();
        digest.update(b"flow-publication-page-index-v2\0");
        digest.update(&content_dependency.0);
        for value in [model.key().as_str(), model.revision().as_str()] {
            digest.update(&(value.len() as u64).to_le_bytes());
            digest.update(value.as_bytes());
        }
        for key in model.reading_order() {
            digest.update(&(key.as_str().len() as u64).to_le_bytes());
            digest.update(key.as_str().as_bytes());
            let document = model.document(key)?;
            let scope = DocumentCacheKey {
                publication: active.key.publication,
                document: key.clone(),
            };
            if document
                .fonts
                .iter()
                .any(|font| !self.fonts.contains_key(&(scope.publication, font.clone())))
            {
                return None;
            }
            let mut viewer = DocumentViewer::new("page-index dependencies", self.settings.clone());
            if !viewer.set_font_resources(self.scoped_fonts(&scope)) {
                return None;
            }
            digest.update(&viewer.persistent_layout_digest());
        }
        let viewport = self.viewer.viewport_size();
        digest.update(&viewport.0.to_le_bytes());
        digest.update(&viewport.1.to_le_bytes());
        Some(*digest.finalize().as_bytes())
    }

    pub fn install_publication_page_index(&mut self, pages: Vec<Option<u32>>) -> bool {
        let Some(active) = &self.active else {
            return false;
        };
        let publication = active.key.publication;
        let Some(model) = self.publications.get(publication) else {
            return false;
        };
        if pages.len() != model.reading_order().len() || pages.contains(&Some(0)) {
            return false;
        }
        self.clear_pagination();
        self.publication_pagination = Some(PublicationPagination {
            publication,
            viewport: self.viewer.viewport_size(),
            pages,
            job: None,
            failure: None,
        });
        true
    }

    pub(super) fn clear_pagination(&mut self) {
        if self.publication_pagination.take().is_some() {
            self.effects
                .push(SessionEffect::PublicationPaginationChanged);
        }
        let ids = self
            .requests
            .iter()
            .filter_map(|(id, request)| {
                (request.purpose() == RequestPurpose::Pagination).then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in ids {
            self.cancel_request(id);
        }
        self.deferred_images
            .retain(|(_, _, _, purpose)| *purpose != RequestPurpose::Pagination);
    }
    pub(super) fn invalidate_document_page_counts(&mut self, key: &DocumentCacheKey) {
        let Some(index) = &mut self.publication_pagination else {
            return;
        };
        if index.publication != key.publication {
            return;
        }
        let model = self
            .publications
            .get(key.publication)
            .expect("registered publication");
        let mut changed = false;
        // Image metadata is document-scoped; learning its dimensions must not
        // discard stable counts for unrelated documents.
        for (document, pages) in model.reading_order().iter().zip(&mut index.pages) {
            if document == &key.document {
                changed |= pages.take().is_some();
            }
        }
        if changed {
            self.effects
                .push(SessionEffect::PublicationPaginationChanged);
        }
    }
    pub(super) fn fail_pagination(&mut self, failure: LoadFailure) {
        if let Some(index) = &mut self.publication_pagination {
            index.failure = Some(failure);
            self.effects
                .push(SessionEffect::PublicationPaginationChanged);
        }
    }

    pub(super) fn accept_pagination_document(
        &mut self,
        key: DocumentCacheKey,
        cached: CachedDocument,
    ) -> Result<(), ReadingSessionError> {
        if self
            .publication_pagination
            .as_ref()
            .is_none_or(|index| index.publication != key.publication || index.job.is_some())
        {
            self.cache_document(key, cached);
            return Ok(());
        }
        let namespace = self.namespace(&key);
        let mut viewer = DocumentViewer::new(key.document.as_str(), self.settings.clone());
        viewer.resize(self.viewer.viewport_size());
        viewer.set_resource_namespace(Some(namespace));
        let document = cached.imported.document;
        let bindings = Arc::new(Bindings {
            links: cached.imported.links,
            resources: cached.imported.resources,
        });
        self.publication_pagination
            .as_mut()
            .expect("index exists")
            .job = Some(PaginationDocument {
            key: key.clone(),
            viewer,
            bindings,
            waiting_metadata: HashSet::new(),
            semantic_key: cached.semantic_key,
        });
        self.apply_known_metadata(&key, &document, true);
        self.publication_pagination
            .as_mut()
            .and_then(|index| index.job.as_mut())
            .expect("job exists")
            .viewer
            .replace_document(Ok(document));
        Ok(())
    }

    pub(super) fn advance_pagination(&mut self) -> Result<(), ReadingSessionError> {
        let Some(index) = self
            .publication_pagination
            .as_ref()
            .filter(|index| index.failure.is_none())
        else {
            return Ok(());
        };
        if index.job.is_none() {
            let Some(next) = index.pages.iter().position(Option::is_none) else {
                return Ok(());
            };
            let key = DocumentCacheKey {
                publication: index.publication,
                document: self.model(index.publication)?.reading_order()[next].clone(),
            };
            let cached = if self.active.as_ref().is_some_and(|active| active.key == key) {
                let active = self.active.as_ref().expect("checked above");
                self.viewer
                    .active_document()
                    .cloned()
                    .map(|document| CachedDocument {
                        imported: ImportedDocument {
                            document,
                            links: active.bindings.links.clone(),
                            resources: active.bindings.resources.clone(),
                        },
                        semantic_key: None,
                    })
            } else if let Some(entry) = self
                .back
                .iter()
                .chain(&self.forward)
                .find(|entry| entry.active.key == key && entry.viewer.is_some())
            {
                entry
                    .viewer
                    .as_ref()
                    .and_then(ViewerSession::document)
                    .cloned()
                    .map(|document| CachedDocument {
                        imported: ImportedDocument {
                            document,
                            links: entry.active.bindings.links.clone(),
                            resources: entry.active.bindings.resources.clone(),
                        },
                        semantic_key: None,
                    })
            } else {
                self.take_cached_document(&key)
            };
            if let Some(cached) = cached {
                self.accept_pagination_document(key, cached)?;
            } else {
                self.request_document(key, RequestPurpose::Pagination, None)?;
                return Ok(());
            }
        }
        let key = self
            .publication_pagination
            .as_ref()
            .and_then(|index| index.job.as_ref())
            .expect("created or returned")
            .key
            .clone();
        if !self.ensure_fonts(&key, RequestPurpose::Pagination, None)? {
            return Ok(());
        }
        let fonts = self.scoped_fonts(&key);
        let job = self
            .publication_pagination
            .as_mut()
            .and_then(|index| index.job.as_mut())
            .expect("job exists");
        if !job.viewer.set_font_resources(fonts) {
            self.fail_pagination(LoadFailure::new("invalid font environment"));
            return Ok(());
        }
        let viewport = job.viewer.viewport_size();
        let count = job
            .viewer
            .prepare_pagination(viewport.0, viewport.1)
            .and_then(|(_, total)| u32::try_from(total).ok());
        let effects = job.viewer.take_effects();
        let requested_metadata = effects
            .iter()
            .any(|effect| matches!(effect, ViewerEffect::RequestImageMetadata(_)));
        for effect in effects {
            if let ViewerEffect::RequestImageMetadata(image) = effect {
                self.publication_pagination
                    .as_mut()
                    .and_then(|index| index.job.as_mut())
                    .expect("job exists")
                    .waiting_metadata
                    .insert(image.url.clone());
                self.request_image(
                    key.clone(),
                    image,
                    ResourceKind::ImageMetadata,
                    RequestPurpose::Pagination,
                )?;
            }
        }
        let ready = self
            .publication_pagination
            .as_ref()
            .and_then(|index| index.job.as_ref())
            .is_some_and(|job| job.waiting_metadata.is_empty());
        if !ready {
            return Ok(());
        }
        if requested_metadata {
            self.effects.push(SessionEffect::ContinueWork);
            return Ok(());
        }
        let Some(count) = count.filter(|count| *count != 0) else {
            self.fail_pagination(LoadFailure::new("pagination failed"));
            return Ok(());
        };
        let slot = self
            .model(key.publication)?
            .reading_order_index(&key.document)
            .expect("job is a reading-order item");
        let index = self.publication_pagination.as_mut().expect("index exists");
        index.pages[slot] = Some(count);
        self.effects
            .push(SessionEffect::PublicationPaginationChanged);
        let job = index.job.take().expect("job exists");
        if !self.resident(&key)
            && let Some(document) = job.viewer.active_document().cloned()
        {
            self.cache_document(
                key,
                CachedDocument {
                    imported: ImportedDocument {
                        document,
                        links: job.bindings.links.clone(),
                        resources: job.bindings.resources.clone(),
                    },
                    semantic_key: job.semantic_key,
                },
            );
        }
        self.trim_fonts();
        if !self.publication_pagination_complete() {
            self.effects.push(SessionEffect::ContinueWork);
        }
        Ok(())
    }
}
