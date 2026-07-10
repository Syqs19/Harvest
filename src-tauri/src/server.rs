//! Modalità server: espone la stessa UI e la coda di download sulla rete
//! di casa, così il telefono può comandare i download dal browser.
//! Protetto da PIN; il traffico non lascia mai la LAN.

use std::collections::HashMap;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State, WebSocketUpgrade,
    },
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use tauri::{AppHandle, Manager};
use tower_http::cors::CorsLayer;

use crate::{begin_download, do_cancel, DownloadState};

/// Porta preferita e riserve, provate in ordine se la precedente è occupata
const PORT_CANDIDATES: [u16; 4] = [7777, 7778, 17771, 27777];

/// Indirizzi a cui il telefono può collegarsi (nome del PC + IP di casa)
pub fn local_addresses(port: u16) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        out.push(format!("http://{}.local:{port}", name.to_lowercase()));
    }
    if let Ok(ip) = local_ip_address::local_ip() {
        out.push(format!("http://{ip}:{port}"));
    }
    out
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let router = Router::new()
            .route("/api/state", get(get_state))
            .route("/api/start", post(post_start))
            .route("/api/cancel", post(post_cancel))
            .route("/api/events", get(ws_events))
            .route("/api/browse", get(get_browse))
            .route("/api/mkdir", post(post_mkdir))
            .fallback(get(static_assets))
            .layer(CorsLayer::very_permissive())
            .with_state(app.clone());

        for port in PORT_CANDIDATES {
            match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
                Ok(listener) => {
                    *app.state::<DownloadState>().0.server_port.lock().unwrap() = Some(port);
                    if let Err(e) = axum::serve(listener, router.clone()).await {
                        eprintln!("Server: errore di esecuzione: {e}");
                    }
                    return;
                }
                Err(e) => {
                    eprintln!("Server: porta {port} non disponibile ({e}), provo la prossima")
                }
            }
        }
        eprintln!("Server: nessuna porta disponibile, modalità remote spenta");
    });
}

/// Il PIN può arrivare come header (chiamate fetch) o query string (WebSocket)
fn authed(app: &AppHandle, headers: &HeaderMap, query: &HashMap<String, String>) -> bool {
    let expected = &app.state::<DownloadState>().0.pin;
    let from_header = headers
        .get("x-pin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let from_query = query.get("pin").map(String::as_str).unwrap_or("");
    from_header == expected || from_query == expected
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "PIN mancante o errato").into_response()
}

async fn get_state(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !authed(&app, &headers, &query) {
        return unauthorized();
    }
    let snapshot = app
        .state::<DownloadState>()
        .0
        .snapshot
        .lock()
        .unwrap()
        .clone();
    Json(snapshot).into_response()
}

async fn get_browse(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !authed(&app, &headers, &query) {
        return unauthorized();
    }
    Json(crate::list_dir(query.get("path").cloned())).into_response()
}

#[derive(serde::Deserialize)]
struct MkdirReq {
    parent: String,
    name: String,
}

async fn post_mkdir(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    Json(req): Json<MkdirReq>,
) -> Response {
    if !authed(&app, &headers, &query) {
        return unauthorized();
    }
    match crate::make_dir(&req.parent, &req.name) {
        Ok(listing) => Json(listing).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartReq {
    links: Vec<String>,
    video: bool,
    images: bool,
    video_mode: String,
    #[serde(default)]
    cookies_browser: String,
    output_dir: String,
}

async fn post_start(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    Json(req): Json<StartReq>,
) -> Response {
    if !authed(&app, &headers, &query) {
        return unauthorized();
    }
    match begin_download(
        &app,
        req.links,
        req.video,
        req.images,
        req.video_mode,
        req.cookies_browser,
        req.output_dir,
    ) {
        Ok(()) => (StatusCode::OK, "Avviato").into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn post_cancel(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !authed(&app, &headers, &query) {
        return unauthorized();
    }
    do_cancel(&app.state::<DownloadState>().0);
    StatusCode::OK.into_response()
}

/// WebSocket: inoltra al telefono gli stessi eventi che alimentano la UI desktop
async fn ws_events(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !authed(&app, &headers, &query) {
        return unauthorized();
    }
    let rx = app.state::<DownloadState>().0.tx.subscribe();
    ws.on_upgrade(move |socket| forward_events(socket, rx))
}

async fn forward_events(mut socket: WebSocket, mut rx: tokio::sync::broadcast::Receiver<String>) {
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(json) => {
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break; // telefono disconnesso
                    }
                }
                // In ritardo sul canale: si perde qualche evento, non è grave
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(_)) => continue, // ping/pong o messaggi ignorati
                _ => break,
            },
        }
    }
}

/// Serve la UI React: dagli asset incorporati nell'app (build di release)
/// o, in sviluppo, dalla cartella dist su disco (serve `npm run build`).
async fn static_assets(State(app): State<AppHandle>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if path.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }

    if let Some(asset) = app.asset_resolver().get(format!("/{path}")) {
        return ([(header::CONTENT_TYPE, asset.mime_type)], asset.bytes).into_response();
    }

    // Fallback di sviluppo: la dist accanto al progetto
    let dev_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dist")
        .join(path);
    if let Ok(bytes) = tokio::fs::read(&dev_path).await {
        return ([(header::CONTENT_TYPE, guess_mime(path))], bytes).into_response();
    }

    (
        StatusCode::NOT_FOUND,
        "UI non trovata: in sviluppo esegui prima `npm run build`.",
    )
        .into_response()
}

fn guess_mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}
