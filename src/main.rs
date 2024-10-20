use std::io::Error as IoError;
use std::net::SocketAddr;
use std::path::Path;

use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket},
        ConnectInfo, Request, State, WebSocketUpgrade,
    },
    http::{header, HeaderMap, StatusCode, Version},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    routing::{any, get, get_service, post},
    Router,
};
use axum_extra::{headers, TypedHeader};
use axum_server::tls_rustls::RustlsConfig;
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
    webserver: Option<WebserverConfig>,
}

#[derive(Deserialize, Debug, Clone)]
struct WebserverConfig {
    bluemaps_path: Option<String>,
    cert_path: Option<String>,
}

#[derive(Clone)]
struct AppState {
    config: WebserverConfig,
    control: MinecraftControl,
}

#[tokio::main]
async fn main() -> Result<(), IoError> {
    let file = fs::read_to_string("config.toml").await.unwrap();
    let config: AppConfig = toml::from_str(&file).unwrap();

    let control = minecraft::init(config.minecraft);

    let webconfig: WebserverConfig = match config.webserver {
        Some(c) => c,
        None => WebserverConfig {
            bluemaps_path: None,
            cert_path: None,
        },
    };
    let state = AppState {
        config: webconfig,
        control,
    };

    let ssl_config: Option<RustlsConfig> = match &state.config.cert_path {
        Some(p) => {
            let cert = Path::new(p).join("cert.pem");
            let key = Path::new(p).join("key.pem");
            Some(RustlsConfig::from_pem_file(cert, key).await.unwrap())
        }
        None => None,
    };

    let assets_dir = Path::new(".").join("assets");
    println!("assets directory: {}", assets_dir.display());

    let map_routes = match &state.config.bluemaps_path {
        Some(p) => {
            let path = Path::new("/").join(p);
            println!("{}", path.display());
            Router::new()
                .route("/map", get(|| async { Redirect::permanent("/map/") }))
                .nest_service(
                    "/map/",
                    ServeDir::new(path)
                        .append_index_html_on_directories(true)
                        .precompressed_gzip(),
                )
        }
        None => Router::new(),
    };

    let app = Router::new()
        .merge(map_routes)
        .fallback_service(ServeDir::new(assets_dir).append_index_html_on_directories(true))
        .route("/ws", any(ws_handler))
        .route("/log", get(log_handler))
        .route("/command", post(command_writer))
        .layer(axum::middleware::from_fn(logging_middleware))
        .with_state(state);

    if ssl_config.is_some() {
        let addr = SocketAddr::from(([0, 0, 0, 0], 443));

        let mut server = axum_server::bind_rustls(addr, ssl_config.unwrap());
        server.http_builder().http2().enable_connect_protocol();
        server.serve(app.into_make_service()).await.unwrap();
    } else {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();

        println!("listening on {}", listener.local_addr().unwrap());
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    }

    Ok(())
}

async fn logging_middleware(request: Request, next: Next) -> Response {
    println!("{}", request.uri());
    let response = next.run(request).await;
    response
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
    version: Version,
    State(mut state): State<AppState>,
) -> impl IntoResponse {
    println!("accepted a WebSocket using {version:?}");
    let rx = state.control.subscribe();
    ws.on_upgrade(move |socket| handle_socket(socket, rx))
}

async fn handle_socket(socket: WebSocket, mut rx: Receiver<String>) {
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
                Some(Ok(_)) => {},
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
