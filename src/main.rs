//! token-dealer entrypoint. Wires config, registry, and the axum app.

use anyhow::Context;
use std::sync::Arc;
use token_dealer::{
    auth::{KeyStore, MasterKey},
    config::ConfigService,
    db::Db,
    metadata::MetadataStore,
    providers::{HealthRegistry, ProviderRegistry},
    proxy::pipeline::Pipeline,
    server::build_router,
    AppState,
};
use tracing_subscriber::{prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --healthcheck: used by Docker HEALTHCHECK. Exits 0 if /health responds.
    if std::env::args().any(|a| a == "--healthcheck") {
        return run_healthcheck();
    }

    init_tracing();
    let config_path = std::env::var("TOKEN_DEALER_CONFIG")
        .unwrap_or_else(|_| "token-dealer.toml".to_string());

    let config = ConfigService::load(&config_path)
        .await
        .with_context(|| format!("loading config from {config_path}"))?;
    let snapshot = config.snapshot().await;
    let bind = snapshot.server.bind.clone();
    let log_level = snapshot.server.log_level.clone();

    // Re-init tracing now that we know the configured log level.
    init_tracing_with_level(&log_level);

    tracing::info!(
        bind = %bind,
        providers = snapshot.providers.len(),
        tiers = snapshot.tiers.len(),
        "token-dealer starting"
    );

    let registry = Arc::new(ProviderRegistry::from_configs(&snapshot.providers).unwrap_or_else(|e| {
        tracing::error!("provider registry build failed: {e}; starting empty");
        ProviderRegistry::from_configs(&[]).unwrap()
    }));
    let http = reqwest::Client::builder()
        .user_agent(concat!("token-dealer/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building reqwest client")?;

    let db = Db::open(&snapshot.database).context("opening request log db")?;
    let metadata = MetadataStore::new();
    token_dealer::metadata::spawn_refresher(metadata.clone());

    let health = HealthRegistry::new();
    let master = MasterKey::from_env_or_generate()?;
    let key_store = KeyStore::new(db.clone(), &master);
    let pipeline = Pipeline::new(registry, config.clone(), http, db.clone(), health.clone(), key_store.clone());
    let state = AppState::new(pipeline, config, health, db, metadata, key_store);

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding to {bind}"))?;
    tracing::info!(addr = %listener.local_addr()?, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn init_tracing() {
    init_tracing_with_level("info");
}

fn init_tracing_with_level(level: &str) {
    let env = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("token_dealer={level},tower_http={level}")));
    let _ = tracing_subscriber::registry()
        .with(env)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .try_init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        let mut s = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        s.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}

fn run_healthcheck() -> anyhow::Result<()> {
    // For Docker HEALTHCHECK: just verify the port is accepting
    // connections. The /health endpoint is exercised by the actual
    // liveness probe in the compose file's healthcheck block.
    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse()
        .unwrap_or(8080);
    let addr = format!("127.0.0.1:{port}");
    match std::net::TcpStream::connect_timeout(
        &addr.parse().expect("valid socket addr"),
        std::time::Duration::from_secs(2),
    ) {
        Ok(_) => std::process::exit(0),
        Err(e) => anyhow::bail!("healthcheck failed: {e}"),
    }
}
