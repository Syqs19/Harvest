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

use crate::{begin_download, discard_queue_inner, do_cancel, resume_queue_inner, DownloadState};

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
            .route("/api/remove", post(post_remove))
            .route("/api/interrupted", get(get_interrupted))
            .route("/api/resume", post(post_resume))
            .route("/api/discard", post(post_discard))
            .route("/api/engine", get(get_engine))
            .route("/api/engine/update", post(post_engine_update))
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

/// Dopo questo numero di PIN errati consecutivi il server si blocca per un po',
/// per rendere impraticabile il brute-force delle 10^6 combinazioni sulla LAN.
const PIN_MAX_FAILS: u32 = 5;
/// Durata del blocco (secondi) una volta superata la soglia.
const PIN_LOCK_SECS: u64 = 30;

/// Secondi da UNIX_EPOCH (orologio di sistema), per la finestra del rate-limit.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Esito della verifica del PIN, con il caso "bloccato" distinto dal PIN errato.
enum Auth {
    Ok,
    BadPin,
    RateLimited,
}

/// Verifica il PIN e aggiorna l'anti-brute-force. Blocca temporaneamente dopo
/// troppi tentativi falliti consecutivi (finestra globale: su una LAN domestica
/// i client legittimi sono pochi, un blocco condiviso è sufficiente e semplice).
fn authed(app: &AppHandle, headers: &HeaderMap, query: &HashMap<String, String>) -> Auth {
    let inner = &app.state::<DownloadState>().0;
    {
        // Se siamo dentro la finestra di blocco, rifiuta senza nemmeno guardare il PIN
        let guard = inner.pin_failures.lock().unwrap();
        if now_secs() < guard.locked_until {
            return Auth::RateLimited;
        }
    }

    let expected = &inner.pin;
    let from_header = headers
        .get("x-pin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let from_query = query.get("pin").map(String::as_str).unwrap_or("");
    let ok = from_header == expected || from_query == expected;

    let mut guard = inner.pin_failures.lock().unwrap();
    if ok {
        guard.fails = 0;
        guard.locked_until = 0;
        Auth::Ok
    } else {
        guard.fails += 1;
        if guard.fails >= PIN_MAX_FAILS {
            guard.locked_until = now_secs() + PIN_LOCK_SECS;
            guard.fails = 0; // riparte da zero dopo lo sblocco
        }
        Auth::BadPin
    }
}

/// Traduce l'esito negativo in risposta HTTP. Restituisce None se autorizzato.
fn deny(auth: Auth) -> Option<Response> {
    match auth {
        Auth::Ok => None,
        Auth::BadPin => Some((StatusCode::UNAUTHORIZED, "Missing or wrong PIN").into_response()),
        Auth::RateLimited => Some(
            (
                StatusCode::TOO_MANY_REQUESTS,
                "Too many wrong attempts, try again shortly",
            )
                .into_response(),
        ),
    }
}

async fn get_state(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
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
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
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
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
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
    /// Contenitore/codec video ("auto" = originale, default per client vecchi)
    #[serde(default = "default_video_format")]
    video_format: String,
    /// Tetto di risoluzione (0 = massima disponibile)
    #[serde(default)]
    max_height: u16,
    #[serde(default)]
    audio_format: String,
    /// Arricchimento: tag/copertina/capitoli (default true per client vecchi)
    #[serde(default = "default_enrich")]
    enrich: bool,
    /// Sottotitoli: "no" | "embed" | "file" | "both" (default "no")
    #[serde(default = "default_subs")]
    subs: String,
    #[serde(default = "default_concurrency")]
    concurrency: u8,
    #[serde(default)]
    cookies_browser: String,
    output_dir: String,
    #[serde(default)]
    append: bool,
}

/// Default per client vecchi che non inviano il campo: 1 = comportamento storico.
fn default_concurrency() -> u8 {
    1
}

fn default_video_format() -> String {
    "auto".into()
}

fn default_enrich() -> bool {
    true
}

fn default_subs() -> String {
    "no".into()
}

async fn post_start(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    Json(req): Json<StartReq>,
) -> Response {
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
    }
    match begin_download(
        &app,
        req.links,
        req.video,
        req.images,
        req.video_mode,
        req.video_format,
        req.max_height,
        req.audio_format,
        req.enrich,
        req.subs,
        req.concurrency,
        req.cookies_browser,
        req.output_dir,
        req.append,
    ) {
        Ok(()) => (StatusCode::OK, "Started").into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct RemoveReq {
    index: usize,
}

async fn post_remove(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    Json(req): Json<RemoveReq>,
) -> Response {
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
    }
    match crate::remove_item_inner(&app.state::<DownloadState>().0, req.index) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// Coda interrotta trovata all'avvio (per il banner Riprendi/Scarta)
async fn get_interrupted(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
    }
    let saved = app
        .state::<DownloadState>()
        .0
        .interrupted
        .lock()
        .unwrap()
        .clone();
    Json(saved).into_response()
}

async fn post_resume(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
    }
    match resume_queue_inner(&app) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn post_discard(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
    }
    discard_queue_inner(&app.state::<DownloadState>().0);
    StatusCode::OK.into_response()
}

/// Versione del motore video e disponibilità di un aggiornamento (non scarica)
async fn get_engine(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
    }
    let dir = app.state::<DownloadState>().0.config_dir.clone();
    Json(crate::engines::check(&app, &dir).await).into_response()
}

/// Aggiorna il motore video all'ultima versione
async fn post_engine_update(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
    }
    let dir = app.state::<DownloadState>().0.config_dir.clone();
    match crate::engines::update(&app, &dir).await {
        Ok(v) => Json(serde_json::json!({ "version": v })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn post_cancel(
    State(app): State<AppHandle>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
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
    if let Some(resp) = deny(authed(&app, &headers, &query)) {
        return resp;
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
    // Blocca la risalita di percorso (..) e i percorsi assoluti: su Windows
    // Path::join sostituirebbe la base con un percorso tipo "C:/..." o "\\server",
    // permettendo di leggere file arbitrari del disco (il fallback dev serve da
    // disco, senza PIN). Un asset legittimo è sempre relativo, senza ':' né '\'.
    if path.contains("..")
        || path.contains(':')
        || path.contains('\\')
        || path.starts_with('/')
    {
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
        "UI not found: in development, run `npm run build` first.",
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
