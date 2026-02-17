pub(super) fn init_tracing() {
    if tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "info,solana_metrics=off,solana_streamer=warn,solana_gossip=off",
                )
            }),
        )
        .try_init()
        .is_err()
    {
        // Tracing was already initialized by embedding host.
    }
}
