use std::net::SocketAddr;
use std::path::Path;
use std::io::Error as IoError;

use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket},
        ConnectInfo, State, WebSocketUpgrade,
    },
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use axum_extra::{headers, TypedHeader};
use futures::{SinkExt, StreamExt};
use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    sync::broadcast::{self, Receiver, Sender},
};
use tokio_tungstenite::tungstenite::Result;
use tokio_util::io::ReaderStream;
use tower_http::services::ServeDir;

mod minecraft;

#[derive(Clone)]
struct AppState {
    tx: Sender<String>,
}

#[tokio::main]
async fn main() -> Result<(), IoError> {
    let (tx, _): (Sender<String>, Receiver<String>) = broadcast::channel(16);
    let mut control = minecraft::new(tx.clone());

    let state = AppState { tx };

    // Create the event loop and TCP listener we'll accept connections on.
    let _ = tokio::task::spawn_blocking(move || control.read_journal());

    let assets_dir = Path::new(".").join("assets");
    println!("assets directory: {}", assets_dir.display());
    // build our application with some routes
    let app = Router::new()
        .fallback_service(ServeDir::new(assets_dir).append_index_html_on_directories(true))
        .route("/ws", get(ws_handler))
        .route("/log", get(log_handler))
        .route("/command", post(command_writer))
        .with_state(state);

    // run it with hyper
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("listening on {}", listener.local_addr().unwrap());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();

    Ok(())
}

async fn log_handler(State(state): State<AppState>) -> impl IntoResponse {
    let file = match tokio::fs::File::open("/var/lib/minecraft/logs/latest.log").await {
        Ok(file) => file,
        Err(err) => return Err((StatusCode::NOT_FOUND, format!("File not found: {}", err))),
    };

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/plain; chatset=utf-8".parse().unwrap(),
    );

    Ok((headers, body))
}

async fn command_writer(State(state): State<AppState>, mut body: String) -> impl IntoResponse {
    let mut file = OpenOptions::new()
        .read(false)
        .write(true)
        .open("/run/minecraft-server.stdin")
        .await
        .unwrap();
    if !body.ends_with("\n") {
        body = format!("{}\n", body);
    }
    let bytes = body.as_bytes();
    let _ = file.write_all(bytes).await.unwrap();
    let _ = file.flush().await.unwrap();
    println!("issued command `{}`", body);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/plain; chatset=utf-8".parse().unwrap(),
    );

    StatusCode::OK
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    user_agent: Option<TypedHeader<headers::UserAgent>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let user_agent = if let Some(TypedHeader(user_agent)) = user_agent {
        user_agent.to_string()
    } else {
        String::from("Unknown browser")
    };
    println!("`{user_agent}` at {addr} connected.");
    let rx = state.tx.subscribe();
    // finalize the upgrade process by returning upgrade callback.
    // we can customize the callback by sending additional info such as address.
    ws.on_upgrade(move |socket| handle_socket(socket, addr, rx))
}

async fn handle_socket(mut socket: WebSocket, who: SocketAddr, mut rx: Receiver<String>) {
    if socket.send(Message::Ping(vec![1, 2, 3])).await.is_ok() {
        println!("Pinged {who}...");
    } else {
        println!("Could not send ping {who}!");
        return;
    }
    let (mut sender, mut receiver) = socket.split();
    loop {
        tokio::select! {
            // Wait for the next message from the broadcast channel
            msg = rx.recv() => match msg {
                Ok(msg) => {
                    // Try to send the message to the WebSocket client
                    if let Err(e) = sender.send(Message::Text(msg)).await {
                        println!("Failed to send message: {}. Closing connection.", e);
                        break;
                    }
                },
                Err(_e) => {}
            },

            // Handle WebSocket close from the client
            result = receiver.next() => match result {
                Some(Ok(x)) => {
                    println!("{}", x.to_text().unwrap());
                }
                Some(Err(e)) => {
                    println!("WebSocket error: {}. Closing connection.", e);
                    break;
                }
                None => {
                    println!("WebSocket closed by client.");
                    break;
                }
            }
        }
    }
}
