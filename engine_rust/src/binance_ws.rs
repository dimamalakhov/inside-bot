//! Binance Futures **WebSocket API** client (`wss://ws-fapi.binance.com/ws-fapi/v1`).
//!
//! Why WS API instead of REST: a signed REST `POST /fapi/v1/order` pays a full
//! TLS handshake cost on a cold connection plus JSON decode of the response;
//! the WS API keeps one warm, authenticated connection and answers in a single
//! frame, which is what this engine relies on for sub-millisecond order
//! submission.
//!
//! Design notes
//! ------------
//! * One background task owns the socket. It reconnects with exponential
//!   backoff and fails all in-flight requests on disconnect, so callers never
//!   hang.
//! * Requests are correlated by monotonically increasing ids; responses are
//!   matched through a lock-free `DashMap`.
//! * `MARKET` orders are sent with `newOrderRespType=RESULT`, which returns
//!   `avgPrice`/`executedQty` in the same response - TP/SL can be placed
//!   immediately, without polling.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, timeout, MissedTickBehavior};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{debug, info, warn};

use crate::math::{
    adjust_price, format_price, format_quantity, stop_limit_price, stop_loss_price,
    take_profit_price,
};
use crate::types::{OrderResponse, OrderStatus, Side, SymbolFilters, TpSlParams};

type HmacSha256 = Hmac<Sha256>;
const PING_INTERVAL: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Order description
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespType {
    Ack,
    Result,
}

impl RespType {
    fn as_str(self) -> &'static str {
        match self {
            RespType::Ack => "ACK",
            RespType::Result => "RESULT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    Market,
    TakeProfit,
    Stop,
}

impl OrderKind {
    fn as_str(self) -> &'static str {
        match self {
            OrderKind::Market => "MARKET",
            OrderKind::TakeProfit => "TAKE_PROFIT",
            OrderKind::Stop => "STOP",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OrderSpec {
    pub symbol: String,
    pub side: Side,
    pub kind: OrderKind,
    pub quantity: String,
    pub price: Option<String>,
    pub stop_price: Option<String>,
    pub time_in_force: Option<&'static str>,
    pub reduce_only: bool,
    pub resp_type: RespType,
}

impl OrderSpec {
    pub fn market(symbol: &str, side: Side, quantity: String) -> Self {
        Self {
            symbol: symbol.to_string(),
            side,
            kind: OrderKind::Market,
            quantity,
            price: None,
            stop_price: None,
            time_in_force: None,
            reduce_only: false,
            resp_type: RespType::Result,
        }
    }

    pub fn take_profit(symbol: &str, side: Side, quantity: String, price: String) -> Self {
        Self {
            symbol: symbol.to_string(),
            side,
            kind: OrderKind::TakeProfit,
            quantity,
            price: Some(price.clone()),
            stop_price: Some(price),
            time_in_force: Some("GTC"),
            reduce_only: true,
            resp_type: RespType::Ack,
        }
    }

    pub fn stop(
        symbol: &str,
        side: Side,
        quantity: String,
        trigger: String,
        limit: String,
    ) -> Self {
        Self {
            symbol: symbol.to_string(),
            side,
            kind: OrderKind::Stop,
            quantity,
            price: Some(limit),
            stop_price: Some(trigger),
            time_in_force: Some("GTC"),
            reduce_only: true,
            resp_type: RespType::Ack,
        }
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

enum PendingKind {
    Order(oneshot::Sender<Result<OrderResponse>>),
    Status(oneshot::Sender<Result<OrderStatus>>),
}

struct Pending {
    kind: PendingKind,
    created: Instant,
    method: &'static str,
}

impl Pending {
    fn fail(self, message: &str) {
        match self.kind {
            PendingKind::Order(tx) => {
                let _ = tx.send(Err(anyhow!(message.to_string())));
            }
            PendingKind::Status(tx) => {
                let _ = tx.send(Err(anyhow!(message.to_string())));
            }
        }
    }
}

enum WsCommand {
    Order {
        spec: OrderSpec,
        tx: oneshot::Sender<Result<OrderResponse>>,
    },
    Status {
        symbol: String,
        order_id: u64,
        tx: oneshot::Sender<Result<OrderStatus>>,
    },
    Shutdown,
}

struct Inner {
    api_key: Arc<str>,
    api_secret: Arc<str>,
    url: String,
    tx: mpsc::UnboundedSender<WsCommand>,
    pending: DashMap<u64, Pending>,
    connected: AtomicBool,
    next_id: AtomicU64,
    order_timeout: Duration,
    recv_window: u64,
    requests_sent: AtomicU64,
    responses_seen: AtomicU64,
}

impl Inner {
    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// Handle to one account's WebSocket API session. Cheap to clone.
#[derive(Clone)]
pub struct BinanceWsClient {
    inner: Arc<Inner>,
}

impl BinanceWsClient {
    pub fn new(
        api_key: String,
        api_secret: String,
        url: String,
        order_timeout: Duration,
        recv_window: u64,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            api_key: api_key.into(),
            api_secret: api_secret.into(),
            url,
            tx,
            pending: DashMap::new(),
            connected: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            order_timeout,
            recv_window,
            requests_sent: AtomicU64::new(0),
            responses_seen: AtomicU64::new(0),
        });

        let runner = inner.clone();
        tokio::spawn(async move { run_connection(runner, rx).await });
        Arc::new(Self { inner })
    }

    #[inline]
    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }

    pub fn pending_requests(&self) -> usize {
        self.inner.pending.len()
    }

    pub fn stats(&self) -> (u64, u64) {
        (
            self.inner.requests_sent.load(Ordering::Relaxed),
            self.inner.responses_seen.load(Ordering::Relaxed),
        )
    }

    /// Sends a signed order request and waits for Binance's reply.
    pub async fn place(&self, spec: OrderSpec) -> Result<OrderResponse> {
        if !self.is_connected() {
            // Fail fast instead of queueing into a dead socket.
            return Err(anyhow!("ws api not connected"));
        }
        let (tx, rx) = oneshot::channel();
        self.inner
            .tx
            .send(WsCommand::Order { spec, tx })
            .map_err(|_| anyhow!("ws client stopped"))?;

        match timeout(self.inner.order_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("ws response channel closed")),
            Err(_) => Err(anyhow!(
                "order request timed out after {:?}",
                self.inner.order_timeout
            )),
        }
    }

    /// `order.status` - only used when a fill had to be resolved out of band.
    pub async fn get_order_status(&self, symbol: &str, order_id: u64) -> Result<OrderStatus> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .tx
            .send(WsCommand::Status {
                symbol: symbol.to_string(),
                order_id,
                tx,
            })
            .map_err(|_| anyhow!("ws client stopped"))?;

        match timeout(self.inner.order_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("ws response channel closed")),
            Err(_) => Err(anyhow!("order.status timed out")),
        }
    }

    pub async fn shutdown(&self) {
        let _ = self.inner.tx.send(WsCommand::Shutdown);
    }
}

// ---------------------------------------------------------------------------
// Connection loop
// ---------------------------------------------------------------------------

async fn run_connection(inner: Arc<Inner>, mut rx: mpsc::UnboundedReceiver<WsCommand>) {
    let mut backoff = Duration::from_millis(200);

    loop {
        info!(url = %inner.url, "connecting to Binance WS API");
        let stream = connect_async(inner.url.as_str()).await;

        let (ws, _response) = match stream {
            Ok(pair) => pair,
            Err(e) => {
                warn!(error = %e, retry_in_ms = backoff.as_millis(), "WS API connect failed");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(15));
                continue;
            }
        };

        info!("WS API connected");
        inner.connected.store(true, Ordering::Release);
        backoff = Duration::from_millis(200);

        let (mut sink, mut stream) = ws.split();
        let mut ping = interval(PING_INTERVAL);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut sweep = interval(Duration::from_secs(5));
        sweep.set_missed_tick_behavior(MissedTickBehavior::Delay);

        let mut shutting_down = false;
        let mut disconnected = false;

        while !disconnected {
            tokio::select! {
                biased;

                command = rx.recv() => {
                    match command {
                        None | Some(WsCommand::Shutdown) => {
                            shutting_down = true;
                            disconnected = true;
                        }
                        Some(command) => {
                            if dispatch(command, &inner, &mut sink).await.is_err() {
                                disconnected = true;
                            }
                        }
                    }
                }

                frame = stream.next() => {
                    match frame {
                        Some(Ok(Message::Text(text))) => handle_text(&inner, &text),
                        Some(Ok(Message::Close(frame))) => {
                            warn!(?frame, "WS API closed by server");
                            disconnected = true;
                        }
                        Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                        Some(Err(e)) => {
                            warn!(error = %e, "WS API read error");
                            disconnected = true;
                        }
                        None => {
                            warn!("WS API stream ended");
                            disconnected = true;
                        }
                        _ => {}
                    }
                }

                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        disconnected = true;
                    }
                }

                _ = sweep.tick() => expire_pending(&inner),
            }
        }

        inner.connected.store(false, Ordering::Release);
        fail_all_pending(&inner, "ws connection lost");

        if shutting_down {
            info!("WS API client stopped");
            return;
        }

        warn!(retry_in_ms = backoff.as_millis(), "WS API disconnected, reconnecting");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(15));
    }
}

async fn dispatch<S>(command: WsCommand, inner: &Inner, sink: &mut S) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
{
    match command {
        WsCommand::Shutdown => Ok(()),
        WsCommand::Order { spec, tx } => {
            let id = inner.next_request_id();
            let params = build_params(inner, &spec);
            let request = json!({
                "id": id.to_string(),
                "method": "order.place",
                "params": params,
            });

            inner.pending.insert(
                id,
                Pending {
                    kind: PendingKind::Order(tx),
                    created: Instant::now(),
                    method: "order.place",
                },
            );

            send_json(inner, sink, id, request).await
        }
        WsCommand::Status {
            symbol,
            order_id,
            tx,
        } => {
            let id = inner.next_request_id();
            let mut params = Map::new();
            params.insert("symbol".into(), json!(symbol));
            params.insert("orderId".into(), json!(order_id));
            params.insert("apiKey".into(), json!(inner.api_key.as_ref()));
            params.insert("timestamp".into(), json!(now_ms()));
            params.insert("recvWindow".into(), json!(inner.recv_window));
            let signature = sign(inner.api_secret.as_ref(), &params);
            params.insert("signature".into(), json!(signature));

            let request = json!({
                "id": id.to_string(),
                "method": "order.status",
                "params": params,
            });

            inner.pending.insert(
                id,
                Pending {
                    kind: PendingKind::Status(tx),
                    created: Instant::now(),
                    method: "order.status",
                },
            );

            send_json(inner, sink, id, request).await
        }
    }
}

async fn send_json<S>(
    inner: &Inner,
    sink: &mut S,
    id: u64,
    request: Value,
) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
{
    let text = serde_json::to_string(&request).context("failed to serialize WS request")?;
    inner.requests_sent.fetch_add(1, Ordering::Relaxed);

    if sink.send(Message::Text(text)).await.is_err() {
        if let Some((_, pending)) = inner.pending.remove(&id) {
            pending.fail("ws send failed");
        }
        return Err(anyhow!("ws send failed"));
    }
    Ok(())
}

fn handle_text(inner: &Inner, text: &str) {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            debug!(error = %e, "non-JSON frame on WS API");
            return;
        }
    };

    // Unsolicited events (no id) are not part of the WS API contract.
    let id = match value.get("id") {
        Some(Value::String(s)) => s.parse::<u64>().ok(),
        Some(Value::Number(n)) => n.as_u64(),
        _ => None,
    };
    let Some(id) = id else {
        debug!("WS API frame without an id, ignored");
        return;
    };

    let Some((_, pending)) = inner.pending.remove(&id) else {
        debug!(id, "WS API response for an expired request");
        return;
    };
    inner.responses_seen.fetch_add(1, Ordering::Relaxed);

    if let Some(error) = value.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let msg = error
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        warn!(id, code, msg, method = pending.method, "Binance rejected request");
        pending.fail(&format!("binance error {code}: {msg}"));
        return;
    }

    let result = match value.get("result") {
        Some(Value::Null) | None => {
            pending.fail("empty result in WS response");
            return;
        }
        Some(result) => result.clone(),
    };

    match pending.kind {
        PendingKind::Order(tx) => match serde_json::from_value::<OrderResponse>(result) {
            Ok(order) => {
                let _ = tx.send(Ok(order));
            }
            Err(e) => {
                warn!(id, error = %e, "cannot parse order.place result");
                let _ = tx.send(Err(anyhow!("cannot parse order response: {e}")));
            }
        },
        PendingKind::Status(tx) => match serde_json::from_value::<OrderStatus>(result) {
            Ok(status) => {
                let _ = tx.send(Ok(status));
            }
            Err(e) => {
                warn!(id, error = %e, "cannot parse order.status result");
                let _ = tx.send(Err(anyhow!("cannot parse order status: {e}")));
            }
        },
    }
}

fn expire_pending(inner: &Inner) {
    let limit = inner.order_timeout + Duration::from_secs(1);
    let now = Instant::now();
    let stale: Vec<u64> = inner
        .pending
        .iter()
        .filter(|entry| now.saturating_duration_since(entry.value().created) > limit)
        .map(|entry| *entry.key())
        .collect();
    for id in stale {
        if let Some((_, pending)) = inner.pending.remove(&id) {
            pending.fail("request timed out inside the ws client");
        }
    }
}

fn fail_all_pending(inner: &Inner, reason: &str) {
    let ids: Vec<u64> = inner.pending.iter().map(|e| *e.key()).collect();
    for id in ids {
        if let Some((_, pending)) = inner.pending.remove(&id) {
            pending.fail(reason);
        }
    }
}

// ---------------------------------------------------------------------------
// Signing
// ---------------------------------------------------------------------------

fn build_params(inner: &Inner, spec: &OrderSpec) -> Map<String, Value> {
    let mut params = Map::new();
    params.insert("symbol".into(), json!(spec.symbol));
    params.insert("side".into(), json!(spec.side.as_str()));
    params.insert("type".into(), json!(spec.kind.as_str()));
    params.insert("quantity".into(), json!(spec.quantity));
    if let Some(price) = &spec.price {
        params.insert("price".into(), json!(price));
    }
    if let Some(stop_price) = &spec.stop_price {
        params.insert("stopPrice".into(), json!(stop_price));
    }
    if let Some(tif) = spec.time_in_force {
        params.insert("timeInForce".into(), json!(tif));
    }
    if spec.reduce_only {
        params.insert("reduceOnly".into(), json!(true));
    }
    params.insert("newOrderRespType".into(), json!(spec.resp_type.as_str()));
    params.insert("apiKey".into(), json!(inner.api_key.as_ref()));
    params.insert("timestamp".into(), json!(now_ms()));
    params.insert("recvWindow".into(), json!(inner.recv_window));

    let signature = sign(inner.api_secret.as_ref(), &params);
    params.insert("signature".into(), json!(signature));
    params
}

/// HMAC-SHA256 over the alphabetically sorted `key=value` pairs, excluding
/// `signature` - exactly what the WS API expects.
fn sign(secret: &str, params: &Map<String, Value>) -> String {
    let query = signing_query(params);
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return String::new(),
    };
    mac.update(query.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn signing_query(params: &Map<String, Value>) -> String {
    let mut pairs: Vec<(&str, String)> = params
        .iter()
        .filter(|(key, _)| key.as_str() != "signature")
        .map(|(key, value)| (key.as_str(), value_to_plain(value)))
        .collect();
    pairs.sort_unstable_by(|a, b| a.0.cmp(b.0));
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn value_to_plain(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// TP / SL bracket
// ---------------------------------------------------------------------------

pub struct BracketResult {
    pub tp_ok: bool,
    pub sl_ok: bool,
    pub tp_error: Option<String>,
    pub sl_error: Option<String>,
}

impl BracketResult {
    pub fn all_ok(&self) -> bool {
        self.tp_ok && self.sl_ok
    }
}

/// Places the protective bracket after a fill. Uses the already-known average
/// price and executed quantity, so no `order.status` polling is needed.
pub struct TpSlManager {
    client: Arc<BinanceWsClient>,
    params: TpSlParams,
}

impl TpSlManager {
    pub fn new(client: Arc<BinanceWsClient>, params: TpSlParams) -> Arc<Self> {
        Arc::new(Self { client, params })
    }

    pub async fn place_bracket(
        &self,
        symbol: &str,
        entry_side: Side,
        avg_price: u64,
        qty: u64,
        filters: &SymbolFilters,
    ) -> BracketResult {
        let mut result = BracketResult {
            tp_ok: false,
            sl_ok: false,
            tp_error: None,
            sl_error: None,
        };

        if avg_price == 0 || qty == 0 {
            result.tp_error = Some("no fill to protect".into());
            result.sl_error = result.tp_error.clone();
            return result;
        }

        // Favorable rounding: TP away from entry, SL toward the trigger side.
        let tp_price = adjust_price(
            take_profit_price(avg_price, self.params.tp_bps, entry_side),
            filters.tick_size,
            entry_side == Side::Buy,
        );
        let sl_trigger = adjust_price(
            stop_loss_price(avg_price, self.params.sl_bps, entry_side),
            filters.tick_size,
            entry_side == Side::Sell,
        );
        let sl_limit = adjust_price(
            stop_limit_price(sl_trigger, self.params.sl_limit_slippage_bps, entry_side),
            filters.tick_size,
            entry_side == Side::Sell,
        );

        if tp_price == 0 || sl_trigger == 0 || sl_limit == 0 {
            result.tp_error = Some("degenerate bracket prices".into());
            result.sl_error = result.tp_error.clone();
            return result;
        }

        let quantity = format_quantity(qty, filters);
        let bracket_side = entry_side.opposite();

        let tp_spec = OrderSpec::take_profit(
            symbol,
            bracket_side,
            quantity.clone(),
            format_price(tp_price, filters),
        );
        let sl_spec = OrderSpec::stop(
            symbol,
            bracket_side,
            quantity,
            format_price(sl_trigger, filters),
            format_price(sl_limit, filters),
        );

        let (tp_res, sl_res) = tokio::join!(self.client.place(tp_spec), self.client.place(sl_spec));

        match tp_res {
            Ok(order) => {
                result.tp_ok = true;
                debug!(symbol, order_id = order.order_id, "TP accepted");
            }
            Err(e) => {
                let message = e.to_string();
                warn!(symbol, error = %message, "TP rejected");
                result.tp_error = Some(message);
            }
        }
        match sl_res {
            Ok(order) => {
                result.sl_ok = true;
                debug!(symbol, order_id = order.order_id, "SL accepted");
            }
            Err(e) => {
                let message = e.to_string();
                warn!(symbol, error = %message, "SL rejected");
                result.sl_error = Some(message);
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_for_test() -> Map<String, Value> {
        let mut params = Map::new();
        params.insert("symbol".into(), json!("BTCUSDT"));
        params.insert("side".into(), json!("BUY"));
        params.insert("type".into(), json!("MARKET"));
        params.insert("quantity".into(), json!("0.001"));
        params.insert("newOrderRespType".into(), json!("RESULT"));
        params.insert("apiKey".into(), json!("KEY"));
        params.insert("timestamp".into(), json!(1700000000000u64));
        params.insert("recvWindow".into(), json!(5000));
        params
    }

    #[test]
    fn signing_query_is_sorted_and_complete() {
        let query = signing_query(&params_for_test());
        assert_eq!(
            query,
            "apiKey=KEY&newOrderRespType=RESULT&quantity=0.001&recvWindow=5000&side=BUY&symbol=BTCUSDT&timestamp=1700000000000&type=MARKET"
        );
    }

    #[test]
    fn signature_ignores_existing_signature_field() {
        let mut params = params_for_test();
        let first = sign("secret", &params);
        params.insert("signature".into(), json!("deadbeef"));
        assert_eq!(first, sign("secret", &params));
        assert_eq!(first.len(), 64);
        assert_ne!(first, sign("other-secret", &params));
    }

    #[test]
    fn order_spec_shapes() {
        let market = OrderSpec::market("BTCUSDT", Side::Buy, "0.01".into());
        assert_eq!(market.kind, OrderKind::Market);
        assert!(!market.reduce_only);
        assert_eq!(market.resp_type, RespType::Result);
        assert!(market.price.is_none());

        let tp = OrderSpec::take_profit("BTCUSDT", Side::Sell, "0.01".into(), "50100".into());
        assert_eq!(tp.kind, OrderKind::TakeProfit);
        assert!(tp.reduce_only);
        assert_eq!(tp.price.as_deref(), Some("50100"));
        assert_eq!(tp.stop_price.as_deref(), Some("50100"));

        let sl = OrderSpec::stop(
            "BTCUSDT",
            Side::Sell,
            "0.01".into(),
            "49900".into(),
            "49850".into(),
        );
        assert_eq!(sl.kind, OrderKind::Stop);
        assert_eq!(sl.price.as_deref(), Some("49850"));
        assert_eq!(sl.stop_price.as_deref(), Some("49900"));
    }

    #[test]
    fn build_params_signs_everything() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let inner = Inner {
            api_key: "KEY".into(),
            api_secret: "SECRET".into(),
            url: String::new(),
            tx,
            pending: DashMap::new(),
            connected: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            order_timeout: Duration::from_secs(1),
            recv_window: 5000,
            requests_sent: AtomicU64::new(0),
            responses_seen: AtomicU64::new(0),
        };

        let spec = OrderSpec::market("BTCUSDT", Side::Buy, "0.001".into());
        let params = build_params(&inner, &spec);

        assert_eq!(params.get("apiKey").and_then(Value::as_str), Some("KEY"));
        assert_eq!(params.get("symbol").and_then(Value::as_str), Some("BTCUSDT"));
        assert_eq!(params.get("side").and_then(Value::as_str), Some("BUY"));
        assert_eq!(params.get("type").and_then(Value::as_str), Some("MARKET"));
        assert_eq!(
            params.get("newOrderRespType").and_then(Value::as_str),
            Some("RESULT")
        );
        let signature = params.get("signature").unwrap().as_str().unwrap().to_string();
        assert_eq!(signature, sign("SECRET", &params));
        assert_eq!(signature.len(), 64);
    }
}
