//! Scoped resource work and environment transitions. No source-format logic.
use super::*;
use flow_document::viewer::{ImageRequest, ViewerEffect};

impl ReadingSession {
    pub(super) fn namespace(&self, key: &DocumentCacheKey) -> String {
        let model = self
            .publications
            .get(key.publication)
            .expect("registered document scope");
        let mut hash = blake3::Hasher::new();
        hash.update(b"flow-document-resource-scope-v1\0");
        for value in [
            model.key().as_str(),
            model.revision().as_str(),
            key.document.as_str(),
        ] {
            hash.update(&(value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
        hash.finalize().to_hex().to_string()
    }

    pub(super) fn ensure_fonts(
        &mut self,
        key: &DocumentCacheKey,
        purpose: RequestPurpose,
        navigation: Option<NavigationId>,
    ) -> Result<bool, ReadingSessionError> {
        let fonts = self
            .model(key.publication)?
            .document(&key.document)
            .ok_or_else(|| ModelError::UnknownDocument(key.document.clone()))?
            .fonts
            .clone();
        let mut ready = true;
        for font in fonts {
            let identity = (key.publication, font.clone());
            if self.fonts.contains_key(&identity) {
                self.font_lru.retain(|candidate| candidate != &identity);
                self.font_lru.push_back(identity);
                continue;
            }
            ready = false;
            if self.failed_fonts.contains(&identity) {
                let failure =
                    LoadFailure::new("required font unavailable; explicit retry required");
                if navigation.is_some() {
                    self.finish_navigation(NavigationState::Failed(failure));
                } else {
                    self.fail_pagination(failure);
                }
                return Ok(false);
            }
            let existing = self.requests.iter_mut().find(|(_, request)| matches!(request,
                PendingRequest::Resource { key: candidate, reference: ResourceReference::Publication(resource), kind: ResourceKind::Font, .. }
                if candidate.publication == key.publication && resource == &font));
            if let Some((
                id,
                PendingRequest::Resource {
                    purpose: previous,
                    navigation: previous_navigation,
                    ..
                },
            )) = existing
            {
                if purpose < *previous {
                    *previous = purpose;
                    self.effects
                        .push(SessionEffect::Reprioritize { id: *id, purpose });
                }
                if navigation.is_some() {
                    *previous_navigation = navigation;
                }
            } else if self.can_request(purpose) {
                self.issue_resource(
                    key.clone(),
                    ResourceReference::Publication(font),
                    ResourceKind::Font,
                    purpose,
                    navigation,
                    Vec::new(),
                )?;
            }
        }
        Ok(ready)
    }

    pub(super) fn scoped_fonts(&self, key: &DocumentCacheKey) -> Vec<FontResource> {
        let mut resources = self.font_resources.clone();
        if let Some(document) = self
            .publications
            .get(key.publication)
            .and_then(|model| model.document(&key.document))
        {
            resources.extend(document.fonts.iter().filter_map(|resource| {
                self.fonts
                    .get(&(key.publication, resource.clone()))
                    .cloned()
            }));
        }
        resources
    }
    pub(super) fn configure_active_environment(
        &mut self,
        key: &DocumentCacheKey,
    ) -> Result<(), ReadingSessionError> {
        if !self.viewer.set_font_resources(self.scoped_fonts(key)) {
            return Err(ReadingSessionError::InvalidResource);
        }
        self.viewer
            .set_resource_namespace(Some(self.namespace(key)));
        Ok(())
    }

    fn issue_resource(
        &mut self,
        key: DocumentCacheKey,
        reference: ResourceReference,
        kind: ResourceKind,
        purpose: RequestPurpose,
        navigation: Option<NavigationId>,
        images: Vec<ImageRequest>,
    ) -> Result<(), ReadingSessionError> {
        let model = self.model(key.publication)?;
        let id = RequestId::new()?;
        let request = ResourceRequest {
            id,
            publication: model.key().clone(),
            revision: model.revision().clone(),
            document: key.document.clone(),
            resource: reference.clone(),
            kind: kind.clone(),
            purpose,
            source_base: images.first().and_then(|image| image.resource_base.clone()),
        };
        self.requests.insert(
            id,
            PendingRequest::Resource {
                key,
                reference,
                kind,
                purpose,
                navigation,
                images,
            },
        );
        self.effects.push(SessionEffect::LoadResource(request));
        Ok(())
    }

    pub(super) fn collect_viewer_effects(&mut self) -> Result<(), ReadingSessionError> {
        let waiting = std::mem::take(&mut self.deferred_images);
        for (key, image, kind, purpose) in waiting {
            self.request_image(key, image, kind, purpose)?;
        }
        for effect in self.viewer.take_effects() {
            match effect {
                ViewerEffect::FollowLink(destination) => {
                    self.follow_destination(destination)?;
                }
                ViewerEffect::RequestImageMetadata(request) => {
                    if let Some(active) = &self.active {
                        self.request_image(
                            active.key.clone(),
                            request,
                            ResourceKind::ImageMetadata,
                            RequestPurpose::Foreground,
                        )?;
                    }
                }
                ViewerEffect::RequestImagePixels(request) => {
                    if let Some(active) = &self.active {
                        let kind = ResourceKind::ImagePixels {
                            desired_width: request.desired_width,
                            desired_height: request.desired_height,
                        };
                        self.request_image(
                            active.key.clone(),
                            request,
                            kind,
                            RequestPurpose::Foreground,
                        )?;
                    }
                }
                effect => self.effects.push(SessionEffect::Viewer(effect)),
            }
        }
        Ok(())
    }

    pub(super) fn request_image(
        &mut self,
        key: DocumentCacheKey,
        request: ImageRequest,
        kind: ResourceKind,
        purpose: RequestPurpose,
    ) -> Result<(), ReadingSessionError> {
        if request.namespace.as_deref() != Some(self.namespace(&key).as_str()) {
            return Ok(());
        }
        if kind == ResourceKind::ImageMetadata
            && let Some((width, height)) = self
                .image_metadata
                .get(&(key.clone(), request.url.clone()))
                .copied()
        {
            self.apply_image_metadata(&key, request, width, height);
            return Ok(());
        }
        if let Some((_, PendingRequest::Resource { images, .. })) = self.requests.iter_mut().find(|(_, pending)| matches!(pending,
            PendingRequest::Resource { key: candidate, images, kind: requested, .. }
            if candidate == &key && requested == &kind && images.iter().any(|image| image == &request))) {
            if !images.contains(&request) { images.push(request); }
            return Ok(());
        }
        if !self.can_request(purpose) {
            // Put work back into the owning viewer's request state; it can be
            // rediscovered on a subsequent host-driven layout/render step.
            if !self
                .deferred_images
                .iter()
                .any(|(candidate, image, requested, _)| {
                    candidate == &key && image == &request && requested == &kind
                })
            {
                self.deferred_images
                    .push_back((key, request, kind, purpose));
            }
            return Ok(());
        }
        let reference = self
            .active
            .as_ref()
            .filter(|active| active.key == key)
            .and_then(|active| active.bindings.resources.get(&request.url))
            .or_else(|| {
                self.publication_pagination
                    .as_ref()
                    .and_then(|index| index.job.as_ref())
                    .filter(|job| job.key == key)
                    .and_then(|job| job.bindings.resources.get(&request.url))
            })
            .cloned()
            .unwrap_or_else(|| ResourceReference::Host(request.url.clone()));
        self.issue_resource(key, reference, kind, purpose, None, vec![request])
    }

    pub fn provide_resource(
        &mut self,
        id: RequestId,
        resource: PreparedResource,
    ) -> Result<(), ReadingSessionError> {
        self.collect_viewer_effects()?;
        let Some(PendingRequest::Resource {
            key,
            reference,
            kind,
            images,
            ..
        }) = self.requests.get(&id)
        else {
            return Err(ReadingSessionError::StaleRequest);
        };
        let key = key.clone();
        let reference = reference.clone();
        let kind = kind.clone();
        let images = images.clone();
        match (&kind, resource) {
            (ResourceKind::Font, PreparedResource::Font(resource)) => {
                let mut probe = DocumentViewer::new("font validation", self.settings.clone());
                if !probe.register_font_resource(resource.clone()) {
                    return Err(ReadingSessionError::InvalidResource);
                }
                let ResourceReference::Publication(reference) = reference else {
                    return Err(ReadingSessionError::ResponseMismatch);
                };
                let identity = (key.publication, reference);
                self.failed_fonts.remove(&identity);
                self.fonts.insert(identity.clone(), resource);
                self.font_lru.retain(|candidate| candidate != &identity);
                self.font_lru.push_back(identity);
            }
            (ResourceKind::ImageMetadata, PreparedResource::ImageMetadata { width, height }) => {
                if width == 0
                    || height == 0
                    || u64::from(width) * u64::from(height) > 64 * 1024 * 1024
                {
                    return Err(ReadingSessionError::InvalidResource);
                }
                for request in images {
                    self.apply_image_metadata(&key, request, width, height);
                }
            }
            (ResourceKind::ImagePixels { .. }, PreparedResource::ImagePixels(pixels)) => {
                for request in images {
                    if self.active.as_ref().is_some_and(|active| active.key == key)
                        && !self
                            .viewer
                            .provide_image_pixels(request.clone(), pixels.clone())
                    {
                        return Err(ReadingSessionError::InvalidResource);
                    }
                    if let Some(job) = self
                        .publication_pagination
                        .as_mut()
                        .and_then(|index| index.job.as_mut())
                        .filter(|job| job.key == key)
                        && !job.viewer.provide_image_pixels(request, pixels.clone())
                    {
                        return Err(ReadingSessionError::InvalidResource);
                    }
                }
            }
            _ => return Err(ReadingSessionError::ResponseMismatch),
        }
        self.requests.remove(&id);
        self.drive_navigation()?;
        self.advance_pagination()?;
        self.trim_fonts();
        Ok(())
    }

    fn apply_image_metadata(
        &mut self,
        key: &DocumentCacheKey,
        request: ImageRequest,
        width: u32,
        height: u32,
    ) {
        let identity = (key.clone(), request.url.clone());
        let changed = self
            .image_metadata
            .insert(identity.clone(), (width, height))
            != Some((width, height));
        self.image_lru.retain(|candidate| candidate != &identity);
        self.image_lru.push_back(identity);
        while self.image_metadata.len() > 256 {
            if let Some(key) = self.image_lru.pop_front() {
                self.image_metadata.remove(&key);
            } else {
                break;
            }
        }
        self.viewer
            .provide_image_metadata(request.clone(), width, height);
        if let Some(job) = self
            .publication_pagination
            .as_mut()
            .and_then(|index| index.job.as_mut())
            .filter(|job| &job.key == key)
        {
            job.waiting_metadata.remove(&request.url);
            job.viewer.provide_image_metadata(request, width, height);
        }
        if changed {
            self.invalidate_page_counts();
        }
    }

    pub(super) fn apply_known_metadata(
        &mut self,
        key: &DocumentCacheKey,
        document: &FlowDocument,
        background: bool,
    ) {
        let namespace = self.namespace(key);
        for (_, resource) in document.resources() {
            if let Some((width, height)) = self
                .image_metadata
                .get(&(key.clone(), resource.identity.clone()))
                .copied()
            {
                let mut request = ImageRequest::new(
                    &resource.identity,
                    document.resource_base().map(str::to_owned),
                );
                request.namespace = Some(namespace.clone());
                if background {
                    if let Some(job) = self
                        .publication_pagination
                        .as_mut()
                        .and_then(|index| index.job.as_mut())
                    {
                        job.viewer.provide_image_metadata(request, width, height);
                    }
                } else {
                    self.viewer.provide_image_metadata(request, width, height);
                }
            }
        }
    }

    pub(super) fn retry_image_in_scope(&mut self, key: &DocumentCacheKey, image: &ImageRequest) {
        // Reset scoped entries for suspended documents too.
        self.viewer.retry_image(image);
        if let Some(job) = self
            .publication_pagination
            .as_mut()
            .and_then(|index| index.job.as_mut())
            .filter(|job| &job.key == key)
        {
            job.viewer.retry_image(image);
        }
    }

    pub fn retry_failed_resources(&mut self) {
        self.failed_fonts.clear();
        let images = std::mem::take(&mut self.failed_images);
        for (key, image) in images {
            self.retry_image_in_scope(&key, &image);
        }
        if let Some(index) = &mut self.publication_pagination {
            index.failure = None;
        }
    }

    pub(super) fn fail_resource(&mut self, request: PendingRequest, failure: LoadFailure) {
        let PendingRequest::Resource {
            key,
            reference,
            kind,
            images,
            navigation,
            purpose,
        } = request
        else {
            return;
        };
        if kind == ResourceKind::Font {
            if let ResourceReference::Publication(resource) = reference {
                self.failed_fonts.insert((key.publication, resource));
            }
            if navigation.is_some_and(|id| self.is_pending(id)) {
                self.finish_navigation(NavigationState::Failed(failure.clone()));
            }
        }
        for image in images {
            if self.active.as_ref().is_some_and(|active| active.key == key)
                || self
                    .publication_pagination
                    .as_ref()
                    .and_then(|index| index.job.as_ref())
                    .is_some_and(|job| job.key == key)
            {
                self.failed_images.push((key.clone(), image.clone()));
            }
            if self.active.as_ref().is_some_and(|active| active.key == key) {
                if kind == ResourceKind::ImageMetadata {
                    self.viewer.fail_image_metadata(&image);
                } else {
                    self.viewer.fail_image_pixels(&image);
                }
            }
            if let Some(job) = self
                .publication_pagination
                .as_mut()
                .and_then(|index| index.job.as_mut())
                .filter(|job| job.key == key)
            {
                if kind == ResourceKind::ImageMetadata {
                    job.viewer.fail_image_metadata(&image);
                } else {
                    job.viewer.fail_image_pixels(&image);
                }
            }
        }
        if purpose == RequestPurpose::Pagination
            || self
                .publication_pagination
                .as_ref()
                .and_then(|index| index.job.as_ref())
                .is_some_and(|job| job.key == key)
        {
            self.fail_pagination(failure);
        }
    }

    pub(super) fn cancel_obsolete_resources(&mut self) {
        let ids = self
            .requests
            .iter()
            .filter_map(|(id, request)| match request {
                PendingRequest::Resource { key, kind, .. }
                    if *kind != ResourceKind::Font
                        && !self
                            .active
                            .as_ref()
                            .is_some_and(|active| &active.key == key)
                        && !self
                            .publication_pagination
                            .as_ref()
                            .and_then(|index| index.job.as_ref())
                            .is_some_and(|job| &job.key == key) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in ids {
            self.cancel_request(id);
        }
        let active = self.active.as_ref().map(|active| &active.key);
        let background = self
            .publication_pagination
            .as_ref()
            .and_then(|index| index.job.as_ref())
            .map(|job| &job.key);
        self.failed_images
            .retain(|(key, _)| Some(key) == active || Some(key) == background);
        self.deferred_images
            .retain(|(key, _, _, _)| Some(key) == active || Some(key) == background);
    }

    pub(super) fn trim_fonts(&mut self) {
        let mut pinned = HashSet::new();
        let mut scopes = self
            .active
            .iter()
            .map(|active| active.key.clone())
            .collect::<Vec<_>>();
        if let Some((handle, target)) = self
            .navigation
            .as_ref()
            .filter(|navigation| navigation.state == NavigationState::Pending)
            .and_then(|navigation| navigation.bound.as_ref())
        {
            scopes.push(DocumentCacheKey {
                publication: *handle,
                document: target.document.clone(),
            });
        }
        if let Some(job) = self
            .publication_pagination
            .as_ref()
            .and_then(|index| index.job.as_ref())
        {
            scopes.push(job.key.clone());
        }
        for key in scopes {
            if let Some(document) = self
                .publications
                .get(key.publication)
                .and_then(|model| model.document(&key.document))
            {
                pinned.extend(
                    document
                        .fonts
                        .iter()
                        .cloned()
                        .map(|font| (key.publication, font)),
                );
            }
        }
        self.failed_fonts.retain(|key| pinned.contains(key));
        let mut removable = self.font_lru.len();
        while self.fonts.len() > 64 && removable != 0 {
            removable -= 1;
            let Some(key) = self.font_lru.pop_front() else {
                break;
            };
            if pinned.contains(&key) {
                self.font_lru.push_back(key);
            } else {
                self.fonts.remove(&key);
            }
        }
    }
}
