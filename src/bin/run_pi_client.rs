use std::sync::Arc;

use mobile_vehicle_counter_pi::config::Config;
use mobile_vehicle_counter_pi::ingest::{self, IngestState};
use mobile_vehicle_counter_pi::sender::{self, SenderSettings};
use mobile_vehicle_counter_pi::store::Store;
use tokio::sync::Notify;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    dotenvy::dotenv().ok();

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("configuration error: {message}");
            std::process::exit(2);
        }
    };

    let store = Store::open(&config.db_path)?;
    let counts = store.counts()?;
    println!(
        "database {}: {} pending, {} delivered, {} failed",
        config.db_path, counts.pending, counts.delivered, counts.failed
    );
    println!("collector: {}", config.collector_url);

    let wake = Arc::new(Notify::new());

    // The sender runs for the life of the process. Stopping it mid-request is
    // safe: the event stays `pending` and the same bytes are sent next time.
    tokio::spawn(sender::run(
        store.clone(),
        SenderSettings {
            url: config.collector_url.clone(),
            token: config.collector_token.clone(),
            backoff_cap: config.backoff_cap,
        },
        config.request_timeout,
        wake.clone(),
    ));

    let app = ingest::router(IngestState { store, wake }, config.max_camera_body_bytes);
    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    println!("listening for cameras on http://{}", config.listen_addr);

    // On SIGTERM / Ctrl-C, finish the camera posts in flight (cameras cannot retry).
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    println!("shut down");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    println!("shutdown requested");
}
