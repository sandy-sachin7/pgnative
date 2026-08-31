//! pgNative desktop entrypoint.
//! One binary for now per product decision — delegates to `pgnative-app::run_native`.

fn main() -> eframe::Result<()> {
    // Structured logging for diagnostics (no secrets — see AGENTS.md §35).
    // `tracing_subscriber::fmt` with env filter is cheap and respects RUST_LOG.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    pgnative_app::run_native()
}
