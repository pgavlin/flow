use std::sync::atomic::{AtomicU64, Ordering};

/// Allocates a process-local identity without wrapping at exhaustion.
pub(crate) fn allocate(counter: &AtomicU64) -> Option<u64> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(1)?;
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Some(current),
            Err(actual) => current = actual,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_increase_and_exhaustion_never_wraps() {
        let counter = AtomicU64::new(1);
        assert_eq!(allocate(&counter), Some(1));
        assert_eq!(allocate(&counter), Some(2));
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate(&counter), Some(u64::MAX - 1));
        assert_eq!(allocate(&counter), None);
        assert_eq!(allocate(&counter), None);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
