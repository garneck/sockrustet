use bytes::Bytes;
use dashmap::DashMap;
use flume::{Receiver, Sender};
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
use tokio::task::JoinHandle;
use warp::{ws::Message, Filter, Rejection, Reply};
use serde::{Deserialize, Serialize};
use futures::{FutureExt, StreamExt};
use dotenv::dotenv;

// Use mimalloc as the global allocator
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// Connection limits - increased for higher throughput
const MAX_CONNECTIONS: usize = 500_000; // Increased from 100k to 500k

// Client connection counter
static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);
static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

// Pre-compile some common messages as static bytes to avoid runtime allocations
static UNAUTHORIZED_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Unauthorized"));
static NOT_FOUND_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Not Found"));
static INTERNAL_ERROR_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Internal Server Error"));
static TOO_MANY_CONNECTIONS_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Too many connections"));

// Optimize sharding for better parallelism and cache efficiency
const SHARD_COUNT: usize = 128; // Increased from 32 to 128 for better distribution
const SHARD_MASK: usize = SHARD_COUNT - 1;

// Message TTL in milliseconds to avoid processing stale messages
const MESSAGE_TTL_MS: u128 = 2000; // 2 seconds
// Maximum channel capacity for backpressure control
const BROADCAST_CHANNEL_CAPACITY: usize = 50_000; // Increased for higher throughput
// Batch size for processing messages
const BATCH_SIZE: usize = 250; // Increased from 100

// Client message type for broadcast optimization
#[derive(Clone)]
struct ClientMessage {
    data: Arc<Bytes>,
    timestamp: Instant,
}

// Our state of currently connected clients - using sharded DashMaps for better concurrency
struct ShardedClients {
    shards: Vec<Arc<DashMap<usize, mpsc::UnboundedSender<Result<Message, warp::Error>>, FxBuildHasher>>>,
    // Track count per shard to avoid scanning empty shards
    counts: [AtomicUsize; SHARD_COUNT],
}

impl ShardedClients {
    fn new() -> Self {
        let mut shards = Vec::with_capacity(SHARD_COUNT);
        for _ in 0..SHARD_COUNT {
            shards.push(Arc::new(DashMap::with_capacity_and_hasher(
                MAX_CONNECTIONS / SHARD_COUNT, 
                FxBuildHasher::default()
            )));
        }
        // Initialize counts array with zeros
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
    fn remove(&self, client_id: &usize) -> Option<(usize, mpsc::UnboundedSender<Result<Message, warp::Error>>)> {
        let shard_idx = *client_id & SHARD_MASK;
        let result = self.shards[shard_idx].remove(client_id);
        if result.is_some() {
            self.counts[shard_idx].fetch_sub(1, Ordering::Relaxed);
        }
        result
    }
    
    // Get shard count for broadcasting optimization
    #[inline(always)]
    fn shard_count(&self, shard_idx: usize) -> usize {
        self.counts[shard_idx].load(Ordering::Relaxed)
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
        // Use a bounded channel with higher capacity for less backpressure
        let (tx, rx) = flume::bounded(BROADCAST_CHANNEL_CAPACITY);
        
        // Start background task for broadcasting - use dedicated thread for better performance
        let _handle = tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(Self::broadcast_task(rx, clients))
        });
        
        Self { tx, _handle }
    }
    
    #[inline(always)]
    async fn send(&self, message: ClientMessage) -> bool {
        // Try sync send first for better performance - avoid async overhead
        match self.tx.try_send(message) {
            Ok(_) => true,
            // Fall back to timeout send if channel is full
            Err(flume::TrySendError::Full(msg)) => {
                match self.tx.send_timeout(msg, std::time::Duration::from_micros(500)) {
                    Ok(_) => true,
                    Err(_) => false,
                }
            }
            Err(_) => false,
        }
    }
    
    async fn broadcast_task(rx: Receiver<ClientMessage>, clients: Clients) {
        // Create a buffer to batch process messages with larger capacity
        let mut buffer = Vec::with_capacity(BATCH_SIZE * 2);
        
        loop {
            // Fill the buffer with available messages
            buffer.clear();
            
            // Collect up to BATCH_SIZE messages without blocking
            let mut received = 0;
            while received < BATCH_SIZE {
                match rx.try_recv() {
                    Ok(msg) => {
                        buffer.push(msg);
                        received += 1;
                    },
                    Err(flume::TryRecvError::Empty) => break,
                    Err(_) => return, // Channel closed
                }
            }
            
            if received == 0 {
                // Use a shorter sleep time for faster response
                tokio::time::sleep(std::time::Duration::from_micros(500)).await;
                continue;
            }
            
            // Process messages in batch with a single connection count check
            let conn_count = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
            if conn_count == 0 {
                continue;  // Skip if no connections
            }
            
            // Process all messages in parallel for much better throughput
            let mut tasks = Vec::with_capacity(received.min(16)); // Limit concurrency
            
            for chunk in buffer.chunks(received / 16.max(1)) {
                let clients = clients.clone();
                let chunk_vec = chunk.to_vec();  // Clone only what's needed
                let conn_count = conn_count;
                
                tasks.push(tokio::spawn(async move {
                    for msg in chunk_vec {
                        // Skip processing if message is too old
                        if msg.timestamp.elapsed().as_millis() > MESSAGE_TTL_MS {
                            continue;
                        }
                        
                        // Convert Bytes to binary message once
                        let message = Message::binary(msg.data.to_vec());
                        
                        // Very optimized broadcast for small connection count
                        if conn_count < 1000 {
                            for shard in &clients.shards {
                                for item in shard.iter() {
                                    let _ = item.value().send(Ok(message.clone()));
                                }
                            }
                            continue;
                        }
                        
                        // Use parallelism for large connection counts
                        let parallelism = if conn_count > 50_000 {
                            32
                        } else if conn_count > 10_000 {
                            16
                        } else {
                            8
                        };
                        
                        // Rest of the broadcast logic remains the same
                        let mut tasks = Vec::with_capacity(parallelism);
                        let shards_per_task = SHARD_COUNT / parallelism;
                        
                        // Distribute shards among worker tasks
                        for i in 0..parallelism {
                            let start_shard = i * shards_per_task;
                            let end_shard = if i == parallelism - 1 {
                                SHARD_COUNT
                            } else {
                                start_shard + shards_per_task
                            };
                            
                            // Skip empty shards
                            let mut has_clients = false;
                            for idx in start_shard..end_shard {
                                if clients.shard_count(idx) > 0 {
                                    has_clients = true;
                                    break;
                                }
                            }
                            
                            if !has_clients {
                                continue;
                            }
                            
                            let clients = clients.clone();
                            let message = message.clone();
                            
                            tasks.push(tokio::spawn(async move {
                                for shard_idx in start_shard..end_shard {
                                    // Skip empty shards
                                    if clients.shard_count(shard_idx) == 0 {
                                        continue;
                                    }
                                    
                                    let shard = &clients.shards[shard_idx];
                                    for item in shard.iter() {
                                        let _ = item.value().send(Ok(message.clone()));
                                    }
                                }
                            }));
                        }
                        
                        // Wait for all broadcast tasks with timeout
                        for task in tasks {
                            let _ = tokio::time::timeout(std::time::Duration::from_millis(100), task).await;
                        }
                    }
                }));
            }
            
            // Wait for all tasks to complete with a reasonable timeout
            for task in tasks {
                let _ = tokio::time::timeout(std::time::Duration::from_millis(50), task).await;
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
        .and(warp::any().map(move || MAX_CONNECTIONS)) // Just pass the constant instead of creating a new semaphore
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
        
    // Stats endpoint for monitoring - optimized with cached responses
    let stats_route = warp::path("stats")
        .map(|| {
            // Use relaxed ordering for stats reading - slight inconsistency is acceptable for stats
            let active = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
            
            // Create response JSON using fixed capacity to avoid reallocations
            let json = format!("{{\"active_connections\":{},\"max_connections\":{}}}", active, MAX_CONNECTIONS);
            
            warp::reply::with_header(
                json,
                "content-type", 
                "application/json"
            )
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
    max_connections: usize,
) -> Result<impl Reply, Rejection> {
    // Check if we're at capacity - use atomic for faster check
    let current = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
    if current >= max_connections {
        return Err(warp::reject::custom(TooManyConnections));
    }
    
    // Increment active connections counter
    ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed); // Use Relaxed for better performance
    
    Ok(ws.on_upgrade(move |socket| {
        handle_websocket(socket, clients)
    }))
}

// Simplified handler without the permit
async fn handle_websocket(
    ws: warp::ws::WebSocket, 
    clients: Clients,
) {
    // The permit will be dropped when this function completes

    // Use a counter to assign a unique ID for this client
    let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);

    // Split the socket into a sender and receive of messages.
    let (ws_tx, mut ws_rx) = ws.split();

    // Create a channel for this client - using a larger buffer for high-throughput
    let (tx, rx) = mpsc::unbounded_channel();

    // Convert messages into a stream of results - with optimized buffer size
    let rx = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);

    // Forward messages from our channel to the WebSocket - optimize with larger buffer
    tokio::task::spawn(rx.forward(ws_tx).map(|_| ()));

    // Add the sender to our clients map - no lock needed with DashMap
    clients.insert(client_id, tx);

    // Handle incoming WebSocket messages - optimized with early return
    while let Some(result) = ws_rx.next().await {
        if result.is_err() {
            break;
        }
    }

    // Client disconnected - use optimized removal
    clients.remove(&client_id);
    
    // Decrement active connections - use Relaxed for better performance
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed); 
}

// Optimize the HTTP POST handler for minimal latency
async fn handle_post(
    post_data: PostData,
    broadcaster: Arc<Broadcaster>,
) -> Result<impl Reply, Rejection> {
    // Fast path: pre-allocate with capacity for better performance
    let mut buf = Vec::with_capacity(post_data.message.len() + 16);
    
    // Manual serialization is faster than using serde_json::to_vec for simple cases
    buf.extend_from_slice(b"{\"message\":\"");
    
    // Escape special characters (minimal escaping for better performance)
    for c in post_data.message.chars() {
        match c {
            '"' => buf.extend_from_slice(b"\\\""),
            '\\' => buf.extend_from_slice(b"\\\\"),
            '\n' => buf.extend_from_slice(b"\\n"),
            '\r' => buf.extend_from_slice(b"\\r"),
            '\t' => buf.extend_from_slice(b"\\t"),
            c => {
                // Fast path for ASCII
                if c as u32 <= 127 {
                    buf.push(c as u8);
                } else {
                    // Handle Unicode characters - rare case
                    let mut tmp = [0u8; 4];
                    let s = c.encode_utf8(&mut tmp);
                    buf.extend_from_slice(s.as_bytes());
                }
            }
        }
    }
    buf.extend_from_slice(b"\"}");
    
    // Create broadcaster message
    let client_message = ClientMessage {
        data: Arc::new(Bytes::from(buf)),
        timestamp: Instant::now(),
    };
    
    // Use non-blocking send for minimal latency impact
    let _ = broadcaster.send(client_message).await;
    
    // Return using Vec<u8> directly which implements Reply
    // To avoid cloning the static response, create a small constant response
    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"success": true})),
        warp::http::StatusCode::OK
    ))
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
