use bytes::Bytes;
use dashmap::DashMap;
use fxhash::FxBuildHasher;
use mimalloc::MiMalloc;
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Instant;
use once_cell::sync::Lazy;
use tokio::sync::mpsc;
use warp::{ws::Message, Filter, Rejection, Reply};
use serde::{Deserialize, Serialize};
use futures::{FutureExt, StreamExt};
use dotenv::dotenv;
use tracing_subscriber;

mod broadcast;
use broadcast::{Broadcaster, ClientMessage};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// Configuration constants
const MAX_CONNECTIONS: usize = 500_000;
const SHARD_COUNT: usize = 128;
const SHARD_MASK: usize = SHARD_COUNT - 1;

// Atomic counters for tracking connections
static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);
static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

// Pre-compile common responses
static UNAUTHORIZED_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"{\"error\":\"Unauthorized\"}"));
static NOT_FOUND_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"{\"error\":\"Not Found\"}"));
static INTERNAL_ERROR_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"{\"error\":\"Internal Error\"}"));
static TOO_MANY_CONNECTIONS_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"{\"error\":\"Too Many Connections\"}"));

// Fast token validation cache
static TOKEN_CACHE: Lazy<DashMap<String, bool, FxBuildHasher>> = Lazy::new(|| 
    DashMap::with_capacity_and_hasher(100, FxBuildHasher::default())
);

// Improved error types for better clarity
#[derive(Debug)]
enum ApiError {
    Unauthorized,
    TooManyConnections,
    Internal(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "Unauthorized"),
            ApiError::TooManyConnections => write!(f, "Too many connections"),
            ApiError::Internal(msg) => write!(f, "Internal server error: {}", msg),
        }
    }
}

impl std::error::Error for ApiError {}
impl warp::reject::Reject for ApiError {}

// Simplified client storage with sharding
pub struct ClientStore {
    shards: Vec<Arc<DashMap<usize, mpsc::UnboundedSender<Result<Message, warp::Error>>, FxBuildHasher>>>,
    counts: [AtomicUsize; SHARD_COUNT],
}

impl ClientStore {
    fn new() -> Self {
        let shards = (0..SHARD_COUNT).map(|_| {
            Arc::new(DashMap::with_capacity_and_hasher(
                MAX_CONNECTIONS / SHARD_COUNT,
                FxBuildHasher::default()
            ))
        }).collect();
        
        let counts = std::array::from_fn(|_| AtomicUsize::new(0));
        Self { shards, counts }
    }

    #[inline(always)]
    fn insert(&self, client_id: usize, sender: mpsc::UnboundedSender<Result<Message, warp::Error>>) {
        let shard_idx = client_id & SHARD_MASK;
        self.shards[shard_idx].insert(client_id, sender);
        self.counts[shard_idx].fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    fn remove(&self, client_id: &usize) -> bool {
        let shard_idx = *client_id & SHARD_MASK;
        let result = self.shards[shard_idx].remove(client_id).is_some();
        if result {
            self.counts[shard_idx].fetch_sub(1, Ordering::Relaxed);
        }
        result
    }
    
    #[inline(always)]
    pub fn shard_count(&self, shard_idx: usize) -> usize {
        self.counts[shard_idx].load(Ordering::Relaxed)
    }
    
    pub fn broadcast(&self, msg: Message, conn_count: usize) {
        // Use adaptive strategy based on connection count
        if conn_count < 1000 {
            self.broadcast_small(msg);
        } else {
            self.broadcast_large(msg);
        }
    }
    
    // Optimized for small number of connections
    #[inline]
    fn broadcast_small(&self, msg: Message) {
        for shard in &self.shards {
            for item in shard.iter() {
                let _ = item.value().send(Ok(msg.clone()));
            }
        }
    }
    
    // Optimized for large number of connections
    fn broadcast_large(&self, msg: Message) {
        // Use rayon or tokio tasks for parallel broadcasting
        // This is a placeholder - actual implementation would depend on performance profiling
        self.broadcast_small(msg);
    }
}

type Clients = Arc<ClientStore>;

// API key management
#[derive(Clone)]
struct ApiKeys {
    emit_keys: Arc<HashSet<String>>,
    subscribe_keys: Arc<HashSet<String>>,
}

// Structs for request/response handling
#[derive(Deserialize, Serialize, Debug, Clone)]
struct PostData {
    message: String,
}

#[derive(Serialize)]
struct StatsResponse {
    active: usize,
    max: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize
    dotenv().ok();
    
    // Setup structured logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    tracing::info!("Starting WebSocket service");
    
    // Setup client storage and broadcaster with optimized configuration
    let clients = Arc::new(ClientStore::new());
    let broadcaster = Arc::new(Broadcaster::new(clients.clone()));
    
    // Load API keys and populate cache
    let api_keys = load_api_keys();
    
    // Create filter factories
    let with_clients = warp::any().map(move || clients.clone());
    let with_keys = warp::any().map(move || api_keys.clone());
    let with_broadcaster = warp::any().map(move || broadcaster.clone());

    // Define routes with improved error handling
    let ws_route = warp::path("subscribe")
        .and(warp::ws())
        .and(warp::query::<HashMap<String, String>>())
        .and(with_keys.clone())
        .and_then(validate_token)
        .and(with_clients.clone())
        .and_then(handle_ws);
        
    let post_route = warp::path("emit")
        .and(warp::post())
        .and(warp::header::<String>("authorization"))
        .and(with_keys.clone())
        .and_then(check_token)
        .untuple_one()
        .and(warp::body::json())
        .and(with_broadcaster)
        .and_then(publish_message);
        
    let stats_route = warp::path("stats")
        .map(|| {
            let active = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
            warp::reply::json(&StatsResponse {
                active,
                max: MAX_CONNECTIONS,
            })
        });
        
    // Add health check route
    let health_route = warp::path("health")
        .map(|| warp::reply::json(&serde_json::json!({"status": "ok"})));

    // Combine routes - include health_route in the combination
    let routes = ws_route
        .or(post_route)
        .or(stats_route)
        .or(health_route) // Health endpoint added to combined routes
        .with(warp::cors().allow_any_origin())
        .recover(handle_errors);

    // Get port from environment variable or use default
    let port = env::var("PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(3030);

    // Start server with optimized settings
    tracing::info!("Server started on http://0.0.0.0:{}", port);

    // Use standard warp server instead of custom socket implementation
    warp::serve(routes)
        .run(([0, 0, 0, 0], port))
        .await;

    Ok(())
}

// Helper functions with more concise implementations
fn load_api_keys() -> ApiKeys {
    let mut emit_keys = HashSet::new();
    let mut subscribe_keys = HashSet::new();
    
    // More idiomatic approach using iterators
    for (env_var, keys_set) in [
        ("EMIT_API_KEYS", &mut emit_keys as &mut HashSet<String>),
        ("SUBSCRIBE_API_KEYS", &mut subscribe_keys as &mut HashSet<String>)
    ] {
        if let Ok(keys_str) = env::var(env_var) {
            keys_set.extend(
                keys_str.split(',')
                    .filter_map(|k| {
                        let k = k.trim();
                        if !k.is_empty() { Some(k.to_string()) } else { None }
                    })
            );
        }
    }
    
    // Add default keys for development
    if (emit_keys.is_empty() || subscribe_keys.is_empty()) && env::var("PRODUCTION").is_err() {
        if emit_keys.is_empty() {
            emit_keys.extend(["emit-dev-key-1", "emit-dev-key-2"].iter().map(ToString::to_string));
        }
        if subscribe_keys.is_empty() {
            subscribe_keys.extend(["subscribe-dev-key-1", "subscribe-dev-key-2"].iter().map(ToString::to_string));
        }
        tracing::warn!("Using development API keys");
    }
    
    // Populate token cache
    TOKEN_CACHE.clear();
    subscribe_keys.iter().for_each(|k| { TOKEN_CACHE.insert(k.clone(), true); });
    
    tracing::info!("Loaded {} emit keys and {} subscribe keys", emit_keys.len(), subscribe_keys.len());
    
    ApiKeys {
        emit_keys: Arc::new(emit_keys),
        subscribe_keys: Arc::new(subscribe_keys),
    }
}

// Combined token validation for WebSocket
async fn validate_token(
    ws: warp::ws::Ws,
    query: HashMap<String, String>,
    api_keys: ApiKeys,
) -> Result<(warp::ws::Ws, ApiKeys), Rejection> {
    let token = query.get("token").ok_or_else(|| warp::reject::custom(ApiError::Unauthorized))?;
    
    if TOKEN_CACHE.contains_key(token) || api_keys.subscribe_keys.contains(token) {
        // Add to cache if not already there
        TOKEN_CACHE.entry(token.clone()).or_insert(true);
        Ok((ws, api_keys))
    } else {
        Err(warp::reject::custom(ApiError::Unauthorized))
    }
}

// Check token for HTTP endpoint
async fn check_token(token: String, api_keys: ApiKeys) -> Result<(), Rejection> {
    // Extract token from Authorization header
    let token = token.strip_prefix("Bearer ").unwrap_or(&token);
    
    if api_keys.emit_keys.contains(token) {
        Ok(())
    } else {
        Err(warp::reject::custom(ApiError::Unauthorized))
    }
}

// WebSocket connection handler with improved error handling
async fn handle_ws(
    ws_and_keys: (warp::ws::Ws, ApiKeys),
    clients: Clients
) -> Result<impl Reply, Rejection> {
    let (ws, _) = ws_and_keys;
    
    // Check connection limit with more detailed errors
    if ACTIVE_CONNECTIONS.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
        tracing::error!("Connection limit reached: {}/{}", ACTIVE_CONNECTIONS.load(Ordering::Relaxed), MAX_CONNECTIONS);
        return Err(warp::reject::custom(ApiError::TooManyConnections));
    }
    
    // Increment connection count and handle upgrade
    ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
    
    Ok(ws.on_upgrade(move |socket| async move {
        let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
        tracing::debug!("Client connected: {}", client_id);
        
        let (ws_tx, mut ws_rx) = socket.split();
        let (tx, rx) = mpsc::unbounded_channel();
        
        tokio::task::spawn(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx)
                .forward(ws_tx)
                .map(move |result| {  // Added 'move' keyword here to capture client_id by value
                    if let Err(e) = result {
                        tracing::debug!("Error sending to client {}: {}", client_id, e);
                    }
                })
        );
        
        clients.insert(client_id, tx);
        
        // Wait for disconnect with better error handling
        while let Some(result) = ws_rx.next().await {
            match result {
                Ok(_) => continue,
                Err(e) => {
                    tracing::debug!("WebSocket error for client {}: {}", client_id, e);
                    break;
                }
            }
        }
        
        // Clean up
        clients.remove(&client_id);
        let count = ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
        tracing::debug!("Client disconnected: {} (active: {})", client_id, count - 1);
    }))
}

// Message publishing handler with improved error messages
async fn publish_message(
    data: PostData,
    broadcaster: Arc<Broadcaster>,
) -> Result<impl Reply, Rejection> {
    // Use Serde to handle JSON formatting and escaping
    let message_json = serde_json::json!({"message": data.message});
    let message_bytes = serde_json::to_vec(&message_json)
        .map_err(|e| warp::reject::custom(ApiError::Internal(e.to_string())))?;
    
    let client_message = ClientMessage {
        data: Arc::new(Bytes::from(message_bytes)),
        timestamp: Instant::now(),
    };
    
    // Send to broadcaster with better error handling
    if !broadcaster.send(client_message).await {
        tracing::warn!("Failed to send message to broadcaster, channel might be full");
    }
    
    Ok(warp::reply::json(&serde_json::json!({"success": true})))
}

// Error handler with proper HTTP status codes
async fn handle_errors(err: warp::Rejection) -> Result<impl Reply, Rejection> {
    let (status, message) = if err.is_not_found() {
        tracing::error!("Not found: {:?}", err);
        (warp::http::StatusCode::NOT_FOUND, NOT_FOUND_MESSAGE.to_vec())
    } else if let Some(ApiError::Unauthorized) = err.find() {
        tracing::debug!("Unauthorized access attempt");
        (warp::http::StatusCode::UNAUTHORIZED, UNAUTHORIZED_MESSAGE.to_vec())
    } else if let Some(ApiError::TooManyConnections) = err.find() {
        tracing::warn!("Too many connections");
        (warp::http::StatusCode::SERVICE_UNAVAILABLE, TOO_MANY_CONNECTIONS_MESSAGE.to_vec())
    } else if let Some(ApiError::Internal(msg)) = err.find() {
        tracing::error!("Internal error: {}", msg);
        (warp::http::StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE.to_vec())
    } else {
        tracing::error!("Unhandled rejection: {:?}", err);
        (warp::http::StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE.to_vec())
    };
    
    Ok(warp::reply::with_status(
        warp::reply::with_header(message, "content-type", "application/json"),
        status,
       ))
}
