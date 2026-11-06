//! Redis queue consumer (`BLPOP trade_queue`).
//!
//! `telegram_listener.py` pushes the raw Telegram text, `bridge_client.py`
//! pushes JSON - both are forwarded verbatim to the dispatcher, which parses
//! them. The connection is created and repaired here, so the dispatcher never
//! has to think about Redis.

use std::time::Duration;

use redis::AsyncCommands;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

pub async fn run(
    redis_url: String,
    queue: String,
    tx: mpsc::Sender<String>,
    mut shutdown: watch::Receiver<bool>,
) {
    let client = match redis::Client::open(redis_url.as_str()) {
        Ok(client) => client,
        Err(e) => {
            error!(error = %e, url = %redis_url, "invalid Redis URL for queue consumer");
            return;
        }
    };

    let mut connection: Option<redis::aio::Connection> = None;
    let mut backoff = Duration::from_millis(200);

    info!(queue = %queue, "queue consumer started");

    loop {
        if *shutdown.borrow() {
            break;
        }

        if connection.is_none() {
            match client.get_async_connection().await {
                Ok(conn) => {
                    info!(queue = %queue, "queue consumer connected to Redis");
                    connection = Some(conn);
                    backoff = Duration::from_millis(200);
                }
                Err(e) => {
                    warn!(error = %e, retry_in_ms = backoff.as_millis(), "queue Redis connect failed");
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = shutdown.changed() => { break; }
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                    continue;
                }
            }
        }

        let conn = connection.as_mut().expect("connection checked above");
        // 1 second block keeps shutdown responsive while still being cheap.
        let popped: redis::RedisResult<Option<(String, String)>> =
            conn.blpop(&queue, 1.0).await;

        match popped {
            Ok(Some((_key, payload))) => {
                if tx.send(payload).await.is_err() {
                    info!("signal dispatcher closed, queue consumer stopping");
                    break;
                }
            }
            Ok(None) => {}
            Err(e) => {
                warn!(error = %e, "BLPOP failed, reconnecting");
                connection = None;
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.changed() => { break; }
                }
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        }
    }

    info!("queue consumer stopped");
}
