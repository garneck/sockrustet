use bytes::{Bytes};
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use once_cell::sync::Lazy;
use tokio::sync::mpsc;
use warp::{ws::Message, Filter, Rejection, Reply};
use serde::{Deserialize, Serialize};
use futures::{FutureExt, StreamExt};
use dotenv::dotenv;

// Client connection counter
static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);

// Pre-compile some common messages
static UNAUTHORIZED_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Unauthorized"));
static NOT_FOUND_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Not Found"));
static INTERNAL_ERROR_MESSAGE: Lazy<Bytes> = Lazy::new(|| Bytes::from_static(b"Internal Server Error"));

// Our state of currently connected clients - using DashMap for better concurrency
// - client_id -> sender of messages
type Clients = Arc<DashMap<usize, mpsc::UnboundedSender<Result<Message, warp::Error>>>>;

// Replace the single API keys set with a struct containing separate sets
#[derive(Clone)]
struct ApiKeySets {
    emit_keys: Arc<HashSet<String>>,    // Keys for POST requests
    subscribe_keys: Arc<HashSet<String>>, // Keys for WebSocket connections
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

#[tokio::main]
async fn main() {
    // Load .env file if available
    dotenv().ok();
    
    // Initialize logging with filter for production
    if env::var("PRODUCTION").is_ok() {
        std::env::set_var("RUST_LOG", "warn");
    } else {
        std::env::set_var("RUST_LOG", "info");
    }
    pretty_env_logger::init();
    
    // Fix for console_subscriber - only initialize if feature is enabled
    #[cfg(feature = "tokio-console")]
    console_subscriber::init();
    
    // Keep track of all connected clients - DashMap is lock-free
    let clients: Clients = Arc::new(DashMap::new());
    
    // Load API keys from environment variables
    let api_key_sets = load_api_keys();
    println!("Loaded {} emit API keys and {} subscribe API keys", 
             api_key_sets.emit_keys.len(), 
             api_key_sets.subscribe_keys.len());
    
    // Turn our state into filters so we can reuse them
    let clients_filter = warp::any().map(move || clients.clone());
    let api_keys_filter = warp::any().map(move || api_key_sets.clone());

    // WebSocket handler with authentication - fixed to use HashMap for query params
    let ws_route = warp::path("subscribe")
        .and(warp::ws())
        .and(warp::query::<HashMap<String, String>>()) // Changed to HashMap for deserialization
        .and(api_keys_filter.clone())
        .and_then(validate_ws_token)
        .and(clients_filter.clone())
        .map(|ws: warp::ws::Ws, clients| {
            ws.on_upgrade(move |socket| handle_websocket(socket, clients))
        });

    // POST handler with authentication
    let post_route = warp::path("emit")
        .and(warp::post())
        .and(warp::header::<String>("authorization"))
        .and(api_keys_filter.clone())
        .and_then(validate_token)
        .untuple_one() // Skip the unit value
        .and(warp::body::json())
        .and(clients_filter.clone())
        .and_then(handle_post);

    // Combined routes
    let routes = ws_route
        .or(post_route)
        .with(warp::cors().allow_any_origin())
        .recover(handle_rejection);

    println!("Server started at http://127.0.0.1:3030");
    println!("WebSocket endpoint: ws://127.0.0.1:3030/subscribe?token=<subscribe-api-key>");
    println!("POST endpoint: http://127.0.0.1:3030/emit (requires Authorization header with emit-api-key)");
    
    // Use hyper's low-level optimizations
    warp::serve(routes)
        .run(([127, 0, 0, 1], 3030))
        .await;
}

// Function to load API keys from environment variables
fn load_api_keys() -> ApiKeySets {
    // Use pre-allocated collections with capacity hints where possible
    let mut emit_keys = HashSet::with_capacity(10);
    let mut subscribe_keys = HashSet::with_capacity(10);
    
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
    
    // Backward compatibility for API_KEYS variable
    if let Ok(api_keys_str) = env::var("API_KEYS") {
        for key in api_keys_str.split(',') {
            let trimmed_key = key.trim();
            if !trimmed_key.is_empty() {
                emit_keys.insert(trimmed_key.to_string());
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

// Validate WebSocket connection token from query parameter - updated to use HashMap
async fn validate_ws_token(
    ws: warp::ws::Ws,
    query: HashMap<String, String>, // Changed to HashMap
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

async fn handle_websocket(ws: warp::ws::WebSocket, clients: Clients) {
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
        if let Err(e) = result {
            eprintln!("WebSocket send error: {}", e);
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
}

async fn handle_post(
    post_data: PostData,
    clients: Clients,
) -> Result<impl Reply, Rejection> {
    // Create the message we'll send to clients - precompute once
    let message_bytes = serde_json::to_vec(&post_data).unwrap_or_default();
    let message = Message::binary(message_bytes);
    
    // Count active clients
    let client_count = clients.len();
    
    // Only broadcast if there are clients
    if client_count > 0 {
        // Optimized broadcast - no lock acquisition
        for item in clients.iter() {
            // Already have a reference, no need to clone for iteration
            let tx = item.value();
            // Try sending, but don't wait or error if can't send
            let _ = tx.send(Ok(message.clone()));
        }
    }
    
    // Return a response to the HTTP client
    Ok(warp::reply::json(&post_data))
}

// Handle rejection (unauthorized) with proper status code
async fn handle_rejection(err: warp::Rejection) -> Result<impl Reply, Rejection> {
    // Use precomputed messages for common responses
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
    } else {
        eprintln!("Unhandled rejection: {:?}", err);
        Ok(warp::reply::with_status(
            warp::reply::html(INTERNAL_ERROR_MESSAGE.clone()),
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        ))
    }
}
