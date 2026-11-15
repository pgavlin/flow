//! Compile-time-selectable phase telemetry.
//!
//! Portable flow code emits only `tracing` spans and events. Applications own
//! subscriber selection and exporter lifecycle. Without the `telemetry` feature,
//! the macros in this module discard their arguments at compile time.
//!
//! Hosts measure elapsed time by observing span lifetimes.

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! phase_span {
    ($name:literal $(, $field:ident = $value:expr)* $(,)?) => {
        $crate::__tracing::info_span!(target: "flow::phase", $name $(, $field = $value)*).entered()
    };
}

#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! phase_span {
    ($($tokens:tt)*) => {
        ()
    };
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! phase_event {
    ($name:literal $(, $field:ident = $value:expr)* $(,)?) => {
        $crate::__tracing::event!(
            name: $name,
            target: "flow::phase",
            $crate::__tracing::Level::INFO
            $(, $field = $value)*
        )
    };
}

#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! phase_event {
    ($($tokens:tt)*) => {
        ()
    };
}

#[doc(hidden)]
pub use crate::phase_event;
#[doc(hidden)]
pub use crate::phase_span;

#[cfg(test)]
mod tests {
    #[cfg(feature = "telemetry")]
    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "Test host installs its subscriber"
    )]
    fn instrumentation_uses_the_host_subscriber() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tracing::{
            Event, Metadata, Subscriber,
            span::{Attributes, Id, Record},
        };
        struct Host(Arc<AtomicUsize>);
        impl Subscriber for Host {
            fn enabled(&self, _: &Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &Attributes<'_>) -> Id {
                self.0.fetch_add(1, Ordering::Relaxed);
                Id::from_u64(1)
            }
            fn record(&self, _: &Id, _: &Record<'_>) {}
            fn record_follows_from(&self, _: &Id, _: &Id) {}
            fn event(&self, _: &Event<'_>) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
            fn enter(&self, _: &Id) {}
            fn exit(&self, _: &Id) {}
        }
        let received = Arc::new(AtomicUsize::new(0));
        tracing::subscriber::with_default(Host(received.clone()), || {
            let _span = super::phase_span!("flow.host.test", value = 1_u64);
            super::phase_event!("flow.host.event", value = 2_u64);
        });
        assert_eq!(received.load(Ordering::Relaxed), 2);
    }

    #[cfg(not(feature = "telemetry"))]
    #[test]
    fn disabled_macros_do_not_evaluate_fields() {
        let evaluated = std::cell::Cell::new(false);
        let _span = super::phase_span!(
            "flow.test",
            value = {
                evaluated.set(true);
                1_u64
            },
        );
        super::phase_event!(
            "flow.test.event",
            value = {
                evaluated.set(true);
                1_u64
            },
        );
        assert!(!evaluated.get());
    }
}
