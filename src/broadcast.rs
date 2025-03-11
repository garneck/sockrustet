use bytes::Bytes;
use flume::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use warp::ws::Message;
use std::sync::atomic::Ordering;
use futures::stream::{self, StreamExt};
use tracing::{debug, warn}; // Removed unused "info"

// Configuration constants for broadcasting - use const expressions for better optimization
const BROADCAST_CHANNEL_CAPACITY: usize = 50_000;
const MESSAGE_TTL_MS: u128 = 2000;
const BATCH_SIZE: usize = 250;
const MAX_PARALLEL_BROADCASTS: usize = 8;

// Client message type with improved memory layout
#[derive(Clone)]
pub struct ClientMessage {
    pub data: Arc<Bytes>,
    pub timestamp: Instant,
}

impl ClientMessage {
    #[inline]
    pub fn is_expired(&self) -> bool {
        self.timestamp.elapsed().as_millis() > MESSAGE_TTL_MS
    }
}

// Message broadcaster with backpressure support
pub struct Broadcaster {
    tx: Sender<ClientMessage>,
    _handle: JoinHandle<()>,
}

impl Broadcaster {
    pub fn new(clients: Arc<crate::ClientStore>) -> Self {
        let (tx, rx) = flume::bounded(BROADCAST_CHANNEL_CAPACITY);
        
        // Start background task for broadcasting with optimized runtime
        let _handle = tokio::task::spawn_blocking(move || {
            // Create dedicated runtime with optimized thread pool for broadcasting
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
                
            rt.block_on(Self::broadcast_task(rx, clients))
        });
        
        Self { tx, _handle }
    }
    
    #[inline]
    pub async fn send(&self, message: ClientMessage) -> bool {
        // Use deadline-based sending for better latency guarantees
        match tokio::time::timeout(
            Duration::from_millis(50),
            async {
                // Try non-blocking send first
                match self.tx.try_send(message) {
                    Ok(_) => true,
                    Err(flume::TrySendError::Full(msg)) => {
                        // Fall back to async send if channel is full
                        self.tx.send_async(msg).await.is_ok()
                    }
                    Err(_) => false,
                }
            }
        ).await {
            Ok(result) => result,
            Err(_) => {
                warn!("Message dropped due to timeout");
                false
            }
        }
    }
    
    async fn broadcast_task(rx: Receiver<ClientMessage>, clients: Arc<crate::ClientStore>) {
        // Use a ring buffer for more efficient batch processing
        let mut buffer = Vec::with_capacity(BATCH_SIZE * 2);
        
        loop {
            // Reset buffer for new batch
            buffer.clear();
            
            // Try to collect a batch with timeout
            match tokio::time::timeout(
                Duration::from_millis(10), 
                Self::collect_batch(&rx, &mut buffer, BATCH_SIZE)
            ).await {
                Ok(_) => {},
                Err(_) => {
                    // Timeout occurred, process whatever we have
                    if buffer.is_empty() {
                        tokio::time::sleep(Duration::from_micros(500)).await;
                        continue;
                    }
                }
            }
            
            if buffer.is_empty() {
                continue;
            }
            
            // Skip if no connections
            let conn_count = crate::ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
            if conn_count == 0 { 
                debug!("No active connections, skipping {} messages", buffer.len());
                continue; 
            }
            
            // Process messages with adaptive parallelism
            Self::process_messages(&buffer, clients.clone(), conn_count).await;
        }
    }
    
    // More efficient batch collection
    async fn collect_batch(
        rx: &Receiver<ClientMessage>, 
        buffer: &mut Vec<ClientMessage>, 
        batch_size: usize
    ) -> usize {
        let mut count = 0;
        
        while count < batch_size {
            match rx.recv_async().await {
                Ok(msg) => {
                    buffer.push(msg);
                    count += 1;
                },
                Err(_) => return count, // Channel closed
            }
        }
        
        count
    }
    
    // Process messages with adaptive parallelization
    async fn process_messages(
        messages: &[ClientMessage], 
        clients: Arc<crate::ClientStore>, 
        conn_count: usize
    ) {
        debug!("Broadcasting {} messages to {} clients", messages.len(), conn_count);
        
        // Filter expired messages first
        let valid_messages: Vec<_> = messages
            .iter()
            .filter(|msg| !msg.is_expired())
            .collect();
            
        if valid_messages.is_empty() {
            return;
        }
        
        // Determine optimal parallelism based on connection count
        let parallel_count = if conn_count > 10_000 {
            MAX_PARALLEL_BROADCASTS
        } else if conn_count > 1_000 {
            4
        } else {
            1
        };
        
        // Use adaptive broadcast strategy
        if parallel_count == 1 {
            // Sequential for small connection counts
            for msg in &valid_messages {
                let ws_message = Message::binary(msg.data.to_vec());
                clients.broadcast(ws_message, conn_count);
            }
        } else {
            // Parallel for large connection counts
            stream::iter(valid_messages)
                .map(|msg| {
                    let ws_message = Message::binary(msg.data.to_vec());
                    let clients = clients.clone();
                    async move {
                        clients.broadcast(ws_message, conn_count);
                    }
                })
                .buffer_unordered(parallel_count)
                .collect::<Vec<_>>()
                .await;
        }
    }
}
