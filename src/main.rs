use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{mpsc, RwLock};
use warp::{ws::Message, Filter, Rejection, Reply};
use serde::{Deserialize, Serialize};
use futures::{FutureExt, StreamExt};

// Client connection counter
static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);

// Our state of currently connected clients
// - client_id -> sender of messages
type Clients = Arc<RwLock<HashMap<usize, mpsc::UnboundedSender<Result<Message, warp::Error>>>>>;

// Struct for HTTP POST data
#[derive(Deserialize, Serialize)]
struct PostData {
    message: String,
}

#[tokio::main]
async fn main() {
    // Initialize logging
    pretty_env_logger::init();

    // Keep track of all connected clients
    let clients = Clients::default();
    // Turn our clients state into a filter so we can reuse it
    let clients_filter = warp::any().map(move || clients.clone());

    // WebSocket handler
    let ws_route = warp::path("subscribe")
        .and(warp::ws())
        .and(clients_filter.clone())
        .map(|ws: warp::ws::Ws, clients| {
            ws.on_upgrade(move |socket| handle_websocket(socket, clients))
        });

    // POST handler
    let post_route = warp::path("emit")
        .and(warp::post())
        .and(warp::body::json())
        .and(clients_filter.clone())
        .and_then(handle_post);

    // Combined routes
    let routes = ws_route
        .or(post_route)
        .with(warp::cors().allow_any_origin());

    println!("Server started at http://127.0.0.1:3030");
    println!("WebSocket endpoint: ws://127.0.0.1:3030/subscribe");
    println!("POST endpoint: http://127.0.0.1:3030/emit");
    
    warp::serve(routes).run(([127, 0, 0, 1], 3030)).await;
}

async fn handle_websocket(ws: warp::ws::WebSocket, clients: Clients) {
    // Use a counter to assign a unique ID for this client
    let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);

    println!("New WebSocket connection: {}", client_id);

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
