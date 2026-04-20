use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use banned_words_service::build_router;
use banned_words_service::config::load;
use banned_words_service::matcher::{Engine, Lang, LIST_VERSION, TERMS};
use banned_words_service::state::AppState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let cfg = load().inspect_err(|e| eprintln!("config error: {e}"))?;

    // M3 loads only `en`; M4 loads every LDNOOBW language subject to `cfg.langs`.
    let loaded: Vec<Lang> = vec!["en".to_string()];
    let mut patterns: HashMap<Lang, &[&str]> = HashMap::new();
    for lang in &loaded {
        let terms = TERMS
            .get(lang.as_str())
            .copied()
            .ok_or_else(|| format!("compiled term table missing language: {lang}"))?;
        patterns.insert(lang.clone(), terms);
    }

    let engine = Arc::new(Engine::new(&patterns));
    let state = Arc::new(AppState {
        engine,
        api_keys: cfg.api_keys,
        list_version: LIST_VERSION,
        ready: AtomicBool::new(false),
        max_inflight: cfg.max_inflight,
    });
    state.ready.store(true, Ordering::Release);

    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(&cfg.listen_addr).await?;
    tracing::info!(
        target: "startup",
        addr = %cfg.listen_addr,
        list_version = LIST_VERSION,
        languages = loaded.len(),
        "Vocab Veto serving"
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json())
        .init();
}
