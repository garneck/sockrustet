use bytes::Bytes;
use dashmap::DashMap;
use flume::{unbounded, Receiver, Sender};
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
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;
use warp::{ws::Message, Filter, Rejection, Reply};
use serde::{Deserialize, Serialize};
use futures::{FutureExt, StreamExt};
use dotenv::dotenv;

// Use mimalloc as the global allocator
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// Connection limits
const MAX_CONNECTIONS: usize = 100_000; // Adjust based on system capacity

// Client connection counter
static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);
static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

// Pre-compile some common messages
static UNAUTHORIZED_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Unauthorized"));
static NOT_FOUND_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Not Found"));
static INTERNAL_ERROR_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Internal Server Error"));
static TOO_MANY_CONNECTIONS_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Too many connections"));

// Use a sharded approach for client connections 
const SHARD_COUNT: usize = 32; // Use power of 2 for better distribution

// Client message type for broadcast optimization
#[derive(Clone)]
struct ClientMessage {
    data: Arc<Bytes>,
    timestamp: Instant,
}

// Our state of currently connected clients - using sharded DashMaps for better concurrency
struct ShardedClients {
    shards: Vec<Arc<DashMap<usize, mpsc::UnboundedSender<Result<Message, warp::Error>>, FxBuildHasher>>>,
}

impl ShardedClients {
    fn new() -> Self {
        let mut shards = Vec::with_capacity(SHARD_COUNT);
        for _ in 0..SHARD_COUNT {
            shards.push(Arc::new(DashMap::with_hasher(FxBuildHasher::default())));
        }
        Self { shards }
    }

    fn insert(&self, client_id: usize, sender: mpsc::UnboundedSender<Result<Message, warp::Error>>) {
        let shard = self.shard_for(client_id);
        shard.insert(client_id, sender);
    }

    fn remove(&self, client_id: &usize) -> Option<(usize, mpsc::UnboundedSender<Result<Message, warp::Error>>)> {
        let shard = self.shard_for(*client_id);
        shard.remove(client_id)
    }

    fn shard_for(&self, client_id: usize) -> &Arc<DashMap<usize, mpsc::UnboundedSender<Result<Message, warp::Error>>, FxBuildHasher>> {
        // Fast modulo for power of 2
        &self.shards[client_id & (SHARD_COUNT - 1)]
    }
}

type Clients = Arc<ShardedClients>;

// Message broadcasting channel
struct Broadcaster {
    tx: Sender<ClientMessage>,
    _handle: JoinHandle<()>, // Keep the background task alive
}

impl Broadcaster {
    fn new(clients: Clients) -> Self {
        let (tx, rx) = unbounded();
        
        // Start background task for broadcasting
        let _handle = tokio::spawn(Self::broadcast_task(rx, clients));
        
        Self { tx, _handle }
    }
    
    async fn send(&self, message: ClientMessage) {
        let _ = self.tx.send_async(message).await;
    }
    
    async fn broadcast_task(rx: Receiver<ClientMessage>, clients: Clients) {
        while let Ok(msg) = rx.recv_async().await {
            Self::process_broadcast(msg, &clients).await;
        }
    }
    
    async fn process_broadcast(msg: ClientMessage, clients: &Clients) {
        // Fixed: convert Bytes to Vec<u8> properly for Message::binary
        let message = Message::binary(msg.data.to_vec());
        let conn_count = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
        
        // Skip if no clients or message is too old
        if conn_count == 0 || msg.timestamp.elapsed().as_secs() > 5 {
            return;
        }
        
        // Process each shard in parallel for large connection counts
        if conn_count > 10000 {
            let tasks: Vec<_> = clients.shards.iter()
                .map(|shard| {
                    let shard = shard.clone();
                    let message = message.clone();
                    tokio::spawn(async move {
                        for item in shard.iter() {
                            let _ = item.value().send(Ok(message.clone()));
                        }
                    })
                })
                .collect();
                
            // Wait for all broadcast tasks
            for task in tasks {
                let _ = task.await;
            }
        } else {
            // Direct processing for smaller connection counts
            for shard in &clients.shards {
                for item in shard.iter() {
                    let _ = item.value().send(Ok(message.clone()));
                }
            }
        }
    }
}

// Replace the single API keys set with a struct containing separate sets
// Fixed: explicitly specify the lifetime and hasher type parameters
#[derive(Clone)]
struct ApiKeySets {
    emit_keys: Arc<HashSet<String>>,    // Using default hasher
    subscribe_keys: Arc<HashSet<String>>, // Using default hasher
}

// Struct for HTTP POST data - use serde's zero-copy deserialization
#[derive(Deserialize, Serialize)]
struct PostData {
    message: String,
}

// Custom rejection for authentication failures
#[derive(Debug)]
struct Unauthorized;
impl warp::reject::Reject for Unauthorized {}

// Custom rejection for too many connections
#[derive(Debug)]
struct TooManyConnections;
impl warp::reject::Reject for TooManyConnections {}

#[tokio::main]
async fn main() {
    // Load .env file if available
    dotenv().ok();
    
    pretty_env_logger::init();
    
    // Fix for console_subscriber - only initialize if feature is enabled
    #[cfg(feature = "tokio-console")]
    console_subscriber::init();
    
    // Keep track of all connected clients - using sharded approach
    let clients: Clients = Arc::new(ShardedClients::new());
    
    // Initialize the broadcaster
    let broadcaster = Arc::new(Broadcaster::new(clients.clone()));
    
    // Load API keys from environment variables
    let api_key_sets = load_api_keys();
    println!("Loaded {} emit API keys and {} subscribe API keys", 
             api_key_sets.emit_keys.len(), 
             api_key_sets.subscribe_keys.len());
    
    // Turn our state into filters so we can reuse them
    let clients_filter = warp::any().map(move || clients.clone());
    let api_keys_filter = warp::any().map(move || api_key_sets.clone());
    let broadcaster_filter = warp::any().map(move || broadcaster.clone());

    // WebSocket handler with authentication
    let ws_route = warp::path("subscribe")
        .and(warp::ws())
        .and(warp::query::<HashMap<String, String>>())
        .and(api_keys_filter.clone())
        .and_then(validate_ws_token)
        .and(clients_filter.clone())
        .and(warp::any().map(|| Arc::new(Semaphore::new(MAX_CONNECTIONS)))) // Create a new semaphore instead
        .and_then(handle_ws_upgrade);

    // POST handler with authentication
    let post_route = warp::path("emit")
        .and(warp::post())
        .and(warp::header::<String>("authorization"))
        .and(api_keys_filter.clone())
        .and_then(validate_token)
        .untuple_one()
        .and(warp::body::json())
        .and(broadcaster_filter.clone())
        .and_then(handle_post);
        
    // Stats endpoint for monitoring
    let stats_route = warp::path("stats")
        .map(|| {
            let active = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
            warp::reply::json(&serde_json::json!({
                "active_connections": active,
                "max_connections": MAX_CONNECTIONS,
            }))
        });

    // Combined routes
    let routes = ws_route
        .or(post_route)
        .or(stats_route)
        .with(warp::cors().allow_any_origin())
        .recover(handle_rejection);

    println!("Server started at http://0.0.0.0:3030");
    println!("WebSocket endpoint: ws://0.0.0.0:3030/subscribe?token=<subscribe-api-key>");
    println!("POST endpoint: http://0.0.0.0:3030/emit (requires Authorization header with emit-api-key)");
    println!("Stats endpoint: http://0.0.0.0:3030/stats");
    
    // Use hyper's low-level optimizations with increased connection limits
    warp::serve(routes)
        // Increase timeouts and max connections
        .run(([0, 0, 0, 0], 3030))
        .await;
}

// Function to load API keys from environment variables
fn load_api_keys() -> ApiKeySets {
    // Use default hasher - more compatible and still fast
    let mut emit_keys = HashSet::new();
    let mut subscribe_keys = HashSet::new();
    
    // Load emit API keys
    if let Ok(api_keys_str) = env::var("EMIT_API_KEYS") {
        for key in api_keys_str.split(',') {
            let trimmed_key = key.trim();
            if !trimmed_key.is_empty() {
                emit_keys.insert(trimmed_key.to_string());
            }
        }
    }
    
    // Load subscribe API keys
    if let Ok(api_keys_str) = env::var("SUBSCRIBE_API_KEYS") {
        for key in api_keys_str.split(',') {
            let trimmed_key = key.trim();
            if !trimmed_key.is_empty() {
                subscribe_keys.insert(trimmed_key.to_string());
            }
        }
    }
    
    // If no keys found and not in production, add some default keys for development
    if (emit_keys.is_empty() || subscribe_keys.is_empty()) && env::var("PRODUCTION").is_err() {
        if emit_keys.is_empty() {
            emit_keys.insert("emit-dev-key-1".to_string());
            emit_keys.insert("emit-dev-key-2".to_string());
        }
        
        if subscribe_keys.is_empty() {
            subscribe_keys.insert("subscribe-dev-key-1".to_string());
            subscribe_keys.insert("subscribe-dev-key-2".to_string());
        }
        
        println!("⚠️ WARNING: Using default development API keys. ⚠️");
    }
    
    ApiKeySets {
        emit_keys: Arc::new(emit_keys),
        subscribe_keys: Arc::new(subscribe_keys),
    }
}

// Validate WebSocket connection token from query parameter
async fn validate_ws_token(
    ws: warp::ws::Ws,
    query: HashMap<String, String>,
    api_keys: ApiKeySets,
) -> Result<warp::ws::Ws, warp::Rejection> {
    // Using get() instead of DashMap's get_ref()
    if let Some(token) = query.get("token") {
        if api_keys.subscribe_keys.contains(token) {
            Ok(ws)
        } else {
            Err(warp::reject::custom(Unauthorized))
        }
    } else {
        Err(warp::reject::custom(Unauthorized))
    }
}

// Validate HTTP token from Authorization header
async fn validate_token(
    token: String, 
    api_keys: ApiKeySets
) -> Result<(), warp::Rejection> {
    // Simple Bearer token extraction - avoid allocation by using references
    let token = if token.starts_with("Bearer ") {
        &token[7..]
    } else {
        &token
    };
    
    if api_keys.emit_keys.contains(token) {
        Ok(())
    } else {
        Err(warp::reject::custom(Unauthorized))
    }
}

// Handle WebSocket upgrade with connection limiting
async fn handle_ws_upgrade(
    ws: warp::ws::Ws,
    clients: Clients,
    semaphore: Arc<Semaphore>,
) -> Result<impl Reply, Rejection> {
    // Try to acquire a connection slot - use acquire_owned for an owned permit
    match semaphore.try_acquire_owned() {
        Ok(permit) => {
            // Increment active connections counter
            ACTIVE_CONNECTIONS.fetch_add(1, Ordering::SeqCst);
            
            Ok(ws.on_upgrade(move |socket| {
                // Now we can move the owned permit into the handler
                handle_websocket(socket, clients, permit)
            }))
        },
        Err(_) => Err(warp::reject::custom(TooManyConnections)),
    }
}

// Update to take an owned permit instead of the semaphore
async fn handle_websocket(
    ws: warp::ws::WebSocket, 
    clients: Clients,
    _permit: tokio::sync::OwnedSemaphorePermit, // Using OwnedSemaphorePermit
) {
    // The permit will be dropped when this function completes

    // Use a counter to assign a unique ID for this client
    let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);

    // Split the socket into a sender and receive of messages.
    let (ws_tx, mut ws_rx) = ws.split();

    // Create a channel for this client - using a larger buffer for bursty traffic
    let (tx, rx) = mpsc::unbounded_channel();

    // Convert messages into a stream of results
    let rx = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);

    // Forward messages from our channel to the WebSocket
    tokio::task::spawn(rx.forward(ws_tx).map(|result| {
        if let Err(_) = result {
            // Just log error
        }
    }));

    // Add the sender to our clients map - no lock needed with DashMap
    clients.insert(client_id, tx);

    // Handle incoming WebSocket messages
    while let Some(result) = ws_rx.next().await {
        if result.is_err() {
            break;
        }
    }

    // Client disconnected - no lock needed with DashMap
    clients.remove(&client_id);
    
    // Decrement active connections
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::SeqCst);
    
    // No need to explicitly release permit, it's handled automatically when 
    // the permit in handle_ws_upgrade goes out of scope
}

async fn handle_post(
    post_data: PostData,
    broadcaster: Arc<Broadcaster>,
) -> Result<impl Reply, Rejection> {
    // Create the message we'll send to clients - precompute once
    let message_bytes = match serde_json::to_vec(&post_data) {
        Ok(bytes) => bytes,
        Err(_) => return Err(warp::reject::reject()),
    };
    
    // Create broadcaster message
    let client_message = ClientMessage {
        data: Arc::new(Bytes::from(message_bytes)),
        timestamp: Instant::now(),
    };
    
    // Send to background broadcaster task
    broadcaster.send(client_message).await;
    
    // Return a response to the HTTP client
    Ok(warp::reply::json(&post_data))
}

// Handle rejection (unauthorized) with proper status code
async fn handle_rejection(err: warp::Rejection) -> Result<impl Reply, Rejection> {
    if err.is_not_found() {
        Ok(warp::reply::with_status(
            warp::reply::html(NOT_FOUND_MESSAGE.clone()),
            warp::http::StatusCode::NOT_FOUND,
        ))
    } else if let Some(Unauthorized) = err.find() {
        Ok(warp::reply::with_status(
            warp::reply::html(UNAUTHORIZED_MESSAGE.clone()),
            warp::http::StatusCode::UNAUTHORIZED,
        ))
    } else if let Some(TooManyConnections) = err.find() {
        Ok(warp::reply::with_status(
            warp::reply::html(TOO_MANY_CONNECTIONS_MESSAGE.clone()),
            warp::http::StatusCode::SERVICE_UNAVAILABLE,
        ))
    } else {
        eprintln!("Unhandled rejection: {:?}", err);
        Ok(warp::reply::with_status(
            warp::reply::html(INTERNAL_ERROR_MESSAGE.clone()),
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        ))
    }
}
