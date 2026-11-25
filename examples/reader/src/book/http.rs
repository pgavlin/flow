//! Host-owned connection pooling and per-book encoded image reuse.
use super::*;
use std::{
    collections::{HashMap, VecDeque},
    sync::OnceLock,
};

pub(super) fn http_client() -> Result<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let _span = crate::telemetry::phase_span!("flow.reader.http_client");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("flow-demo-reader/0.0")
        .build()?;
    Ok(CLIENT.get_or_init(|| client))
}

/// Keep metadata and pixel requests from downloading the same resource twice.
/// Scoped to one open book, with FIFO eviction at the demo's 64 MiB byte limit.
#[derive(Default)]
pub(super) struct EncodedResources {
    entries: HashMap<String, Arc<[u8]>>,
    order: VecDeque<String>,
    bytes: usize,
}
impl EncodedResources {
    pub fn get(&self, key: &str) -> Option<Arc<[u8]>> {
        self.entries.get(key).cloned()
    }
    pub fn insert(&mut self, key: String, bytes: Arc<[u8]>) {
        if bytes.len() > MAX_INPUT || self.entries.contains_key(&key) {
            return;
        }
        while self.bytes + bytes.len() > MAX_INPUT {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(previous) = self.entries.remove(&oldest) {
                self.bytes -= previous.len();
            }
        }
        self.bytes += bytes.len();
        self.order.push_back(key.clone());
        self.entries.insert(key, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoded_resources_are_shared_bounded_and_book_scoped() {
        let mut cache = EncodedResources::default();
        let bytes: Arc<[u8]> = vec![1; MAX_INPUT / 2 + 1].into();
        cache.insert("one".into(), Arc::clone(&bytes));
        assert!(Arc::ptr_eq(&cache.get("one").unwrap(), &bytes));
        cache.insert("two".into(), Arc::clone(&bytes));
        assert!(cache.get("one").is_none());
        assert!(cache.get("two").is_some());
        assert!(cache.bytes <= MAX_INPUT);
        assert!(EncodedResources::default().get("two").is_none());
    }
}
