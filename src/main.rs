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
use minecraft::MinecraftControl;
use serde::Deserialize;
use tokio::{fs, sync::broadcast::Receiver};
use tokio_tungstenite::tungstenite::Result;
use tower_http::services::ServeDir;

mod minecraft;

#[derive(Deserialize, Debug, Clone)]
struct AppConfig {
    minecraft: Option<minecraft::MinecraftConfig>,
    webserver: Option<WebserverConfig>
}

#[derive(Deserialize, Debug, Clone)]
struct WebserverConfig {}

#[derive(Clone)]
struct AppState {
    config: WebserverConfig,
    control: MinecraftControl
}

#[tokio::main]
async fn main() -> Result<(), IoError> {
    let file = fs::read_to_string("config.toml").await.unwrap();
    let config: AppConfig = toml::from_str(&file).unwrap();

    let control = minecraft::init(config.minecraft);

    let webconfig: WebserverConfig = match config.webserver {
        Some(c) => c,
        None => WebserverConfig{},
    };
    let state = AppState { config: webconfig, control };

    let assets_dir = Path::new(".").join("assets");
    println!("assets directory: {}", assets_dir.display());

    let app = Router::new()
        .fallback_service(ServeDir::new(assets_dir).append_index_html_on_directories(true))
        .route("/ws", get(ws_handler))
        .route("/log", get(log_handler))
        .route("/command", post(command_writer))
        .with_state(state);

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
    let logstream = match state.control.log().await {
        Ok(s) => s,
        Err(_) => return Err(""),
    };
    let body = Body::from_stream(logstream);

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/plain; chatset=utf-8".parse().unwrap(),
    );

    Ok((headers, body))
}

async fn command_writer(State(state): State<AppState>, body: String) -> impl IntoResponse {
    match state.control.command(body).await {
        Ok(_) => return StatusCode::OK,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    user_agent: Option<TypedHeader<headers::UserAgent>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(mut state): State<AppState>,
) -> impl IntoResponse {
    let user_agent = if let Some(TypedHeader(user_agent)) = user_agent {
        user_agent.to_string()
    } else {
        String::from("Unknown browser")
    };
    println!("`{user_agent}` at {addr} connected.");
    let rx = state.control.subscribe();
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
