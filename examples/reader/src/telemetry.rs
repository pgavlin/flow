//! Optional desktop phase logging. Neither engine crate installs a subscriber.
pub use flow_publication::document::telemetry::{phase_event, phase_span};

#[cfg(all(feature = "telemetry", not(target_arch = "wasm32")))]
pub fn init() -> anyhow::Result<()> {
    use tracing_subscriber::{
        EnvFilter,
        fmt::{format::FmtSpan, time::Uptime},
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("flow::phase=info")),
        )
        .with_timer(Uptime::default())
        .with_ansi(false)
        .with_thread_names(true)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| anyhow::anyhow!("Could not initialize reader tracing: {error}"))
}

#[cfg(any(not(feature = "telemetry"), target_arch = "wasm32"))]
pub fn init() -> anyhow::Result<()> {
    Ok(())
}
