use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{mpsc, RwLock};
use warp::{ws::Message, Filter, Rejection, Reply};
use serde::{Deserialize, Serialize};
use futures::{FutureExt, StreamExt};
use dotenv::dotenv;

// Client connection counter
static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);

// Our state of currently connected clients
// - client_id -> sender of messages
type Clients = Arc<RwLock<HashMap<usize, mpsc::UnboundedSender<Result<Message, warp::Error>>>>>;

// Replace the single API keys set with a struct containing separate sets
#[derive(Clone)]
struct ApiKeySets {
    emit_keys: Arc<HashSet<String>>,    // Keys for POST requests
    subscribe_keys: Arc<HashSet<String>>, // Keys for WebSocket connections
}

// Struct for HTTP POST data
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
    
    // Initialize logging
    pretty_env_logger::init();

    // Keep track of all connected clients
    let clients = Clients::default();
    
    // Load API keys from environment variables (now returns a struct with separate sets)
    let api_key_sets = load_api_keys();
    println!("Loaded {} emit API keys and {} subscribe API keys", 
             api_key_sets.emit_keys.len(), 
             api_key_sets.subscribe_keys.len());
    
    // Turn our state into filters so we can reuse them
    let clients_filter = warp::any().map(move || clients.clone());
    let api_keys_filter = warp::any().map(move || api_key_sets.clone());

    // WebSocket handler with authentication - fixed function ordering
    let ws_route = warp::path("subscribe")
        .and(warp::ws())
        .and(warp::query::<HashMap<String, String>>()) // For API key in query param
        .and(api_keys_filter.clone())
        .and_then(validate_ws_token) // This now returns the ws object when successful
        .and(clients_filter.clone())
        .map(|ws: warp::ws::Ws, clients| {
            ws.on_upgrade(move |socket| handle_websocket(socket, clients))
        });

    // POST handler with authentication - fixed argument mismatch
    let post_route = warp::path("emit")
        .and(warp::post())
        .and(warp::header::<String>("authorization"))
        .and(api_keys_filter.clone())
        .and_then(validate_token)
        .map(|_| ())  // Replace unit value with a new unit value (cleaner for readability)
        .untuple_one() // Remove the unit value from the filter chain
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
    
    warp::serve(routes).run(([127, 0, 0, 1], 3030)).await;
}

// Function to load API keys from environment variables - updated to load separate sets
fn load_api_keys() -> ApiKeySets {
    let mut emit_keys = HashSet::new();
    let mut subscribe_keys = HashSet::new();
    
    // Load emit API keys
    if let Ok(api_keys_str) = env::var("EMIT_API_KEYS") {
        for key in api_keys_str.split(',') {
            let trimmed_key = key.trim();
            if !trimmed_key.is_empty() {
                emit_keys.insert(trimmed_key.to_string());
                println!("Added emit API key from environment variable");
            }
        }
    }
    
    // Load subscribe API keys
    if let Ok(api_keys_str) = env::var("SUBSCRIBE_API_KEYS") {
        for key in api_keys_str.split(',') {
            let trimmed_key = key.trim();
            if !trimmed_key.is_empty() {
                subscribe_keys.insert(trimmed_key.to_string());
                println!("Added subscribe API key from environment variable");
            }
        }
    }
    
    // Backward compatibility for API_KEYS variable
    if let Ok(api_keys_str) = env::var("API_KEYS") {
        println!("Note: Using legacy API_KEYS environment variable which applies to both endpoints");
        println!("Consider using EMIT_API_KEYS and SUBSCRIBE_API_KEYS instead for better security");
        
        for key in api_keys_str.split(',') {
            let trimmed_key = key.trim();
            if !trimmed_key.is_empty() {
                emit_keys.insert(trimmed_key.to_string());
                subscribe_keys.insert(trimmed_key.to_string());
                println!("Added shared API key from environment variable");
            }
        }
    }
    
    // If no keys found and not in production, add some default keys for development
    if (emit_keys.is_empty() || subscribe_keys.is_empty()) && env::var("PRODUCTION").is_err() {
        println!("No API keys found in environment variables. Adding default development keys.");
        
        if emit_keys.is_empty() {
            emit_keys.insert("emit-dev-key-1".to_string());
            emit_keys.insert("emit-dev-key-2".to_string());
        }
        
        if subscribe_keys.is_empty() {
            subscribe_keys.insert("subscribe-dev-key-1".to_string());
            subscribe_keys.insert("subscribe-dev-key-2".to_string());
        }
        
        println!("⚠️ WARNING: Using default development API keys. ⚠️");
        println!("Set EMIT_API_KEYS and SUBSCRIBE_API_KEYS environment variables for production use.");
    }
    
    ApiKeySets {
        emit_keys: Arc::new(emit_keys),
        subscribe_keys: Arc::new(subscribe_keys),
    }
}

// Validate WebSocket connection token from query parameter - updated to pass through the ws object
async fn validate_ws_token(
    ws: warp::ws::Ws,
    query: HashMap<String, String>,
    api_keys: ApiKeySets,
) -> Result<warp::ws::Ws, warp::Rejection> {
    if let Some(token) = query.get("token") {
        if api_keys.subscribe_keys.contains(token) {
            Ok(ws) // Return the ws object on success
        } else {
            Err(warp::reject::custom(Unauthorized))
        }
    } else {
        Err(warp::reject::custom(Unauthorized))
    }
}

// Validate HTTP token from Authorization header - updated to check emit_keys
async fn validate_token(
    token: String, 
    api_keys: ApiKeySets
) -> Result<(), warp::Rejection> {
    // Simple Bearer token extraction
    let token = match token.strip_prefix("Bearer ") {
        Some(t) => t,
        None => &token,
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

    println!("New authenticated WebSocket connection: {}", client_id);

    // Split the socket into a sender and receive of messages.
    let (ws_tx, mut ws_rx) = ws.split();

    // Create a channel for this client
    let (tx, rx) = mpsc::unbounded_channel();

    // Convert messages into a stream of results
    let rx = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);

    // Forward messages from our channel to the WebSocket
    tokio::task::spawn(rx.forward(ws_tx).map(|result| {
        if let Err(e) = result {
            eprintln!("WebSocket send error: {}", e);
        }
    }));

    // Add the sender to our clients list
    clients.write().await.insert(client_id, tx);

    // Handle incoming WebSocket messages
    while let Some(result) = ws_rx.next().await {
        match result {
            Ok(_) => {
                // Client sent a message, we're not handling client -> server messages in this example
            }
            Err(e) => {
                eprintln!("WebSocket error: {}", e);
                break;
            }
        }
    }

    // Client disconnected
    clients.write().await.remove(&client_id);
    println!("WebSocket client {} disconnected", client_id);
}

async fn handle_post(
    post_data: PostData,
    clients: Clients,
) -> Result<impl Reply, Rejection> {
    // Create the message we'll send to clients
    let msg = serde_json::to_string(&post_data).unwrap();

    broadcast_message(&clients, &msg).await;
    
    // Return a response to the HTTP client
    Ok(warp::reply::json(&post_data))
}

async fn broadcast_message(clients: &Clients, msg: &str) {
    let message = Message::text(msg);

    // Acquire a read lock on the clients HashMap
    let clients_lock = clients.read().await;

    // Send the message to all connected clients
    for (client_id, tx) in clients_lock.iter() {
        if let Err(_) = tx.send(Ok(message.clone())) {
            println!("Error sending message to client {}", client_id);
        }
    }
}

// Handle rejection (unauthorized) with proper status code
async fn handle_rejection(err: warp::Rejection) -> Result<impl Reply, Rejection> {
    if err.is_not_found() {
        Ok(warp::reply::with_status(
            "Not Found",
            warp::http::StatusCode::NOT_FOUND,
        ))
    } else if let Some(Unauthorized) = err.find() {
        Ok(warp::reply::with_status(
            "Unauthorized",
            warp::http::StatusCode::UNAUTHORIZED,
        ))
    } else {
        eprintln!("Unhandled rejection: {:?}", err);
        Ok(warp::reply::with_status(
            "Internal Server Error",
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        ))
    }
}
