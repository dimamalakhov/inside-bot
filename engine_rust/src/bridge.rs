//! UDP bridge receiver (Europe listener -> Tokyo trader).
//!
//! `bridge_client.py` sends the same JSON payload 3 times for redundancy; the
//! deduplicator in the executor absorbs the copies, so this layer just forwards
//! every packet as fast as possible and never blocks on a slow consumer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

/// Optional Redis fan-out: keeps the legacy "UDP -> trade_queue" behaviour for
/// a second consumer (e.g. the Python engine running in duel mode).
pub struct RedisForward {
    pub connection: redis::aio::MultiplexedConnection,
    pub queue: String,
}

pub async fn run(
    port: u16,
    tx: mpsc::Sender<String>,
    forward: Option<RedisForward>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let address = format!("0.0.0.0:{port}");
    let socket = UdpSocket::bind(&address)
        .await
        .with_context(|| format!("failed to bind UDP bridge on {address}"))?;

    info!(%address, "UDP bridge listening");

    let mut buffer = [0u8; 2048];
    let dropped = AtomicU64::new(0);

    loop {
        let received = tokio::select! {
            received = socket.recv_from(&mut buffer) => received,
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
        };

        match received {
            Ok((size, peer)) => {
                let payload = match std::str::from_utf8(&buffer[..size]) {
                    Ok(text) => text.trim(),
                    Err(e) => {
                        warn!(%peer, error = %e, "udp payload is not utf-8");
                        continue;
                    }
                };
                if payload.is_empty() {
                    continue;
                }

                debug!(%peer, payload, "udp signal");

                if let Some(forward) = forward.as_ref() {
                    let mut connection = forward.connection.clone();
                    let queue = forward.queue.clone();
                    let payload = payload.to_string();
                    tokio::spawn(async move {
                        let result: redis::RedisResult<i64> = redis::cmd("RPUSH")
                            .arg(&queue)
                            .arg(payload)
                            .query_async(&mut connection)
                            .await;
                        if let Err(e) = result {
                            debug!(error = %e, "udp bridge could not forward to Redis");
                        }
                    });
                }

                if tx.try_send(payload.to_string()).is_err() {
                    // Never block the socket: signals are redundant, a full
                    // dispatcher means we are already behind.
                    let count = dropped.fetch_add(1, Ordering::Relaxed) + 1;
                    if count % 100 == 1 {
                        warn!(dropped = count, "signal channel full, dropping udp packet");
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "udp receive failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }

    info!(dropped = dropped.load(Ordering::Relaxed), "UDP bridge stopped");
    Ok(())
}
