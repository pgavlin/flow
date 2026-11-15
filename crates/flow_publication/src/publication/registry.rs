//! Generic session registration. No acquisition, URL routing, or format state.

use super::{Publication, PublicationKey, PublicationRevision};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Engine-issued, process-local identity. Never persist or parse as an address.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PublicationHandle(u64);

#[derive(Debug, Default)]
pub struct PublicationRegistry {
    entries: HashMap<PublicationHandle, Arc<Publication>>,
    revisions: HashMap<(PublicationKey, PublicationRevision), PublicationHandle>,
    latest: HashMap<PublicationKey, PublicationHandle>,
}

impl PublicationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        publication: Publication,
    ) -> Result<PublicationHandle, RegistryError> {
        let identity = (publication.key().clone(), publication.revision().clone());
        if let Some(handle) = self.revisions.get(&identity) {
            if self
                .entries
                .get(handle)
                .is_none_or(|entry| **entry != publication)
            {
                return Err(RegistryError::RevisionConflict);
            }
            self.latest.insert(publication.key().clone(), *handle);
            return Ok(*handle);
        }
        let handle = PublicationHandle(
            NEXT_HANDLE
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| RegistryError::Exhausted)?,
        );
        self.latest.insert(publication.key().clone(), handle);
        self.revisions.insert(identity, handle);
        self.entries.insert(handle, Arc::new(publication));
        Ok(handle)
    }

    pub fn get(&self, handle: PublicationHandle) -> Option<&Publication> {
        self.entries.get(&handle).map(Arc::as_ref)
    }
    pub fn latest(&self, key: &PublicationKey) -> Option<PublicationHandle> {
        self.latest.get(key).copied()
    }
    pub fn revision(
        &self,
        key: &PublicationKey,
        revision: &PublicationRevision,
    ) -> Option<PublicationHandle> {
        self.revisions
            .get(&(key.clone(), revision.clone()))
            .copied()
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RegistryError {
    #[error("runtime publication identities exhausted")]
    Exhausted,
    #[error("a different description already uses this publication key/revision")]
    RevisionConflict,
    #[error("unknown runtime publication handle")]
    UnknownPublication,
}
