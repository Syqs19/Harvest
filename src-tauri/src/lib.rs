// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod dedup;
mod resolver;
mod server;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

#[derive(Clone, serde::Serialize)]
struct TaskInfo {
    url: String,
    engine: String,
}

/// Esito di un singolo task: distingue il fallimento vero dal caso
/// "questo motore non ha niente da scaricare qui" (es. gallery-dl su un video)
#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Ok,
    Failed,
    Nothing,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Failed => "failed",
            Outcome::Nothing => "nothing",
        }
    }
}

/// Eventi inviati alla UI durante i download (canale "download-event",
/// e in copia al WebSocket della modalità server)
#[derive(Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum DlEvent {
    QueueStart {
        tasks: Vec<TaskInfo>,
    },
    ItemStart {
        index: usize,
        total: usize,
        url: String,
        engine: String,
    },
    Progress {
        index: usize,
        percent: f64,
    },
    Line {
        index: usize,
        line: String,
    },
    ItemDone {
        index: usize,
        outcome: String,
    },
    Finished {
        ok: usize,
        failed: usize,
        nothing: usize,
        cancelled: bool,
    },
}

/// Fotografia della coda corrente, per chi si collega dal telefono
/// a download già in corso (o finiti).
#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    busy: bool,
    timeline: Vec<SnapTask>,
    last_output_dir: String,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapTask {
    url: String,
    engine: String,
    status: String,
}

pub struct Inner {
    running: AtomicBool,
    cancelled: AtomicBool,
    current_child: Mutex<Option<CommandChild>>,
    /// Eventi serializzati in JSON, inoltrati ai WebSocket collegati
    pub tx: tokio::sync::broadcast::Sender<String>,
    pub snapshot: Mutex<Snapshot>,
    pub pin: String,
    /// Porta su cui il server è riuscito ad aprirsi (None = server non attivo)
    pub server_port: Mutex<Option<u16>>,
    /// True quando l'utente ha scelto "Esci": la X allora chiude davvero
    pub quitting: AtomicBool,
    /// URL già scaricati in questa sessione di download: evita di riscaricare la
    /// stessa immagine/video quando compare in più pagine di un bulk (es. forum
    /// con pagine cumulative). Ripulito all'avvio di ogni nuova coda.
    pub seen_urls: Mutex<std::collections::HashSet<String>>,
    /// Deduplica per contenuto: file identici (A) e immagini percettivamente
    /// uguali anche se in formati/qualità diverse (B).
    pub dedup: dedup::Dedup,
}

#[derive(Clone)]
pub struct DownloadState(pub Arc<Inner>);

impl Inner {
    /// Tiene aggiornata la fotografia della coda man mano che arrivano gli eventi
    fn apply(&self, ev: &DlEvent) {
        let mut snap = self.snapshot.lock().unwrap();
        match ev {
            DlEvent::QueueStart { tasks } => {
                snap.busy = true;
                snap.timeline = tasks
                    .iter()
                    .map(|t| SnapTask {
                        url: t.url.clone(),
                        engine: t.engine.clone(),
                        status: "pending".into(),
                    })
                    .collect();
            }
            DlEvent::ItemStart { index, .. } => {
                if let Some(t) = snap.timeline.get_mut(*index) {
                    t.status = "running".into();
                }
            }
            DlEvent::ItemDone { index, outcome } => {
                if let Some(t) = snap.timeline.get_mut(*index) {
                    t.status = outcome.clone();
                }
            }
            DlEvent::Finished { cancelled, .. } => {
                snap.busy = false;
                if *cancelled {
                    for t in &mut snap.timeline {
                        if t.status == "pending" || t.status == "running" {
                            t.status = "failed".into();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn percent_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[download\]\s+([\d.]+)%").unwrap())
}

fn emit(app: &AppHandle, event: DlEvent) {
    let state = app.state::<DownloadState>();
    state.0.apply(&event);
    if let Ok(json) = serde_json::to_string(&event) {
        let _ = state.0.tx.send(json);
    }
    let _ = app.emit("download-event", event);
}

/// Ritardo casuale tra un download e l'altro (1.5 - 4 s) per non
/// innescare blocchi anti-bot. Basato sull'orologio, evita dipendenze extra.
fn jitter() -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    Duration::from_millis(1500 + nanos % 2500)
}

/// Cartella dove vivono i sidecar (accanto all'eseguibile dell'app),
/// da passare a yt-dlp perché trovi ffmpeg e possa unire video+audio
/// alla massima risoluzione.
fn ffmpeg_dir() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    dir.join(name)
        .exists()
        .then(|| dir.to_string_lossy().into_owned())
}

/// Nome del browser accettato dai motori per `--cookies-from-browser`.
/// Vuoto o sconosciuto = nessun login (comportamento di default).
fn cookies_arg(cookies_browser: &str) -> Option<&'static str> {
    match cookies_browser {
        // Solo Firefox: i browser Chromium blindano i cookie (Chrome 127+)
        "firefox" => Some("firefox"),
        _ => None,
    }
}

/// Siti "a profilo": quando compaiono come link ANNIDATI in un thread, gallery-dl
/// tende a scaricarne l'intero profilo. Li saltiamo in quel caso (ma restano
/// scaricabili se l'utente incolla direttamente il link del profilo).
/// Elenco allungabile all'occorrenza.
const PROFILE_CATEGORIES: &[&str] = &[
    "tiktok",
    "instagram",
    "twitter",
    "x",
    "reddit",
    "pinterest",
    "tumblr",
    "youtube",
    "fanbox",
    "patreon",
];

/// Se la riga è un errore di "login richiesto" da un sito terzo linkato
/// (un social dentro un thread di forum), restituisce un messaggio chiaro.
fn skipped_third_party(line: &str) -> Option<String> {
    if line.contains("AuthorizationError") || line.contains("redirect to login") {
        // Messaggio neutro, senza nominare il sito
        return Some("Saltato: un link richiede un login non disponibile".into());
    }
    None
}

/// Traduce le righe dei motori in stati NEUTRI da mostrare all'utente, senza mai
/// esporre nomi di host, URL o percorsi. Restituisce None per le righe da nascondere.
fn neutral_status(line: &str) -> Option<String> {
    let l = line.to_lowercase();
    if l.contains("error") && !l.contains("authorizationerror") {
        Some("Un elemento non è stato scaricato".into())
    } else if l.contains("[merger]") || l.contains("merging") {
        Some("Unisco audio e video...".into())
    } else if l.contains("extracting") || l.contains("retrieving") {
        Some("Analizzo la pagina...".into())
    } else {
        // Tutto il resto (percorsi di file salvati, info di debug) resta nascosto
        None
    }
}

/// Scarica un singolo link con il sidecar giusto.
async fn run_one(
    app: &AppHandle,
    inner: &Inner,
    index: usize,
    url: &str,
    engine: &str,
    video_mode: &str,
    cookies_browser: &str,
    output_dir: &str,
) -> Outcome {
    let cookies = cookies_arg(cookies_browser);
    let (bin, args): (&str, Vec<String>) = if engine == "video" {
        let mut args = vec![
            "--newline".into(),
            "--no-warnings".into(),
            "-P".into(),
            output_dir.into(),
        ];
        match video_mode {
            // Solo la traccia video, senza audio
            "videoOnly" => {
                args.push("-f".into());
                args.push("bv*".into());
            }
            // Solo l'audio, estratto nel formato migliore
            "audioOnly" => args.push("-x".into()),
            // "full": video+audio, il default di yt-dlp (unisce con ffmpeg)
            _ => {}
        }
        // NB: NIENTE cookie per i video. Su YouTube i cookie di un account loggato
        // fanno scattare controlli anti-bot che bloccano il download ("No video
        // formats found"). Il login serve solo alle immagini (gallery-dl).
        if let Some(dir) = ffmpeg_dir() {
            args.push("--ffmpeg-location".into());
            args.push(dir);
        }
        args.push(url.into());
        ("yt-dlp", args)
    } else {
        // --sleep: pausa casuale tra una richiesta e l'altra dentro la stessa
        // galleria, per non farsi bloccare con 429 Too Many Requests.
        // -R 1: un solo tentativo per file, così i link a siti che richiedono
        // il loro login vengono scartati subito invece di ritentare.
        // --chapter-filter: quando un thread di forum linka a un PROFILO intero
        // di un social, NON ci sprofonda dentro scaricando migliaia di file.
        // I singoli post, i file host e le immagini passano.
        let profile_sites = PROFILE_CATEGORIES
            .iter()
            .map(|c| format!("'{c}'"))
            .collect::<Vec<_>>()
            .join(",");
        let mut args = vec![
            "--sleep".into(),
            "1.0-3.0".into(),
            "-R".into(),
            "1".into(),
            "--chapter-filter".into(),
            format!("category not in ({profile_sites})"),
            // Task immagini: tiene solo i file-immagine diretti, salta i video
            // (i video del forum arrivano dal task "video" via resolver).
            "--filter".into(),
            "extension in ('jpg','jpeg','png','webp','gif','bmp','tiff','avif')".into(),
        ];
        if let Some(b) = cookies {
            args.push("--cookies-from-browser".into());
            args.push(b.into());
        }
        // -D (destinazione esatta) invece di -d: mette i file direttamente nella
        // cartella scelta, senza creare sottocartelle col nome del sito.
        args.push("-D".into());
        args.push(output_dir.into());
        args.push(url.into());
        ("gallery-dl", args)
    };

    let cmd = match app.shell().sidecar(bin) {
        Ok(c) => c.args(args),
        Err(e) => {
            emit(
                app,
                DlEvent::Line {
                    index,
                    line: format!("Errore sidecar {bin}: {e}"),
                },
            );
            return Outcome::Failed;
        }
    };

    let (mut rx, child) = match cmd.spawn() {
        Ok(pair) => pair,
        Err(e) => {
            emit(
                app,
                DlEvent::Line {
                    index,
                    line: format!("Impossibile avviare {bin}: {e}"),
                },
            );
            return Outcome::Failed;
        }
    };
    *inner.current_child.lock().unwrap() = Some(child);

    let mut exit_code: Option<i32> = None;
    let mut unsupported = false;
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                let line = String::from_utf8_lossy(&bytes).trim().to_string();
                if line.is_empty() {
                    continue;
                }
                // yt-dlp segnala così un sito che non sa gestire
                if line.contains("Unsupported URL") || line.contains("is not a valid URL") {
                    unsupported = true;
                }
                if let Some(cap) = percent_re().captures(&line) {
                    if let Ok(p) = cap[1].parse::<f64>() {
                        emit(app, DlEvent::Progress { index, percent: p });
                    }
                }
                // Errore noto dei cookie Chromium: lo traduco in un messaggio utile
                if line.contains("DPAPI")
                    || line.contains("could not find") && line.contains("cookies")
                {
                    emit(app, DlEvent::Line {
                        index,
                        line: "Cookie del browser non leggibili (protezione di Chrome/Edge/Brave). Prova con Firefox o senza login.".into(),
                    });
                    continue;
                }
                // Link che richiede un login non disponibile: messaggio neutro.
                if let Some(msg) = skipped_third_party(&line) {
                    emit(app, DlEvent::Line { index, line: msg });
                    continue;
                }
                // Le righe grezze dei motori contengono nomi di host, URL e percorsi:
                // NON le mostriamo all'utente. Traduciamo solo gli stati utili.
                if let Some(msg) = neutral_status(&line) {
                    emit(app, DlEvent::Line { index, line: msg });
                }
            }
            CommandEvent::Terminated(payload) => {
                exit_code = payload.code;
            }
            _ => {}
        }
    }
    *inner.current_child.lock().unwrap() = None;

    if inner.cancelled.load(Ordering::SeqCst) {
        return Outcome::Failed;
    }

    // Alcuni host mostrano il media (immagine o video) solo via JavaScript, così
    // gallery-dl non lo prende: lo risolviamo con una WebView nascosta e lo
    // scarichiamo noi. want dipende dal tipo scelto dall'utente. Vedi resolver.rs.
    let want = match engine {
        "video" => resolver::Want::Video,
        _ => resolver::Want::Image,
    };
    let extra_ok =
        resolve_extra_media(app, inner, index, url, cookies_browser, output_dir, want).await;

    if exit_code == Some(0) {
        return Outcome::Ok;
    }
    // gallery-dl usa un codice a bit: il bit 64 significa "nessun estrattore
    // per questo URL", cioè niente da scaricare per questo motore
    if unsupported || (bin == "gallery-dl" && exit_code.is_some_and(|c| c & 64 != 0)) {
        // Se comunque abbiamo preso media extra, il task ha prodotto qualcosa
        return if extra_ok {
            Outcome::Ok
        } else {
            Outcome::Nothing
        };
    }
    if extra_ok {
        return Outcome::Ok;
    }
    Outcome::Failed
}

/// Registra un URL come "scaricato" e dice se è la PRIMA volta che lo si vede in
/// questa coda. La chiave ignora il query-string (token/scadenze variabili), così
/// lo stesso file da pagine diverse viene riconosciuto come duplicato.
fn first_time(inner: &Inner, url: &str) -> bool {
    let key = url.split('?').next().unwrap_or(url).to_string();
    inner.seen_urls.lock().unwrap().insert(key)
}

/// Risolve e scarica i media caricati via JS che gallery-dl non prende (immagini
/// o video, secondo `want`). Restituisce true se ha salvato almeno un file.
/// Log neutri, senza nomi di siti.
async fn resolve_extra_media(
    app: &AppHandle,
    inner: &Inner,
    index: usize,
    source_url: &str,
    cookies_browser: &str,
    output_dir: &str,
    want: resolver::Want,
) -> bool {
    let all = resolver::list_thread_links(app, source_url, cookies_browser).await;
    if all.is_empty() {
        return false;
    }

    let want_video = want == resolver::Want::Video;

    // Classifica i link: file diretti del tipo giusto (scaricabili subito) e
    // pagine web da aprire col browser. Le pagine le apriamo SOLO se l'indizio
    // dal percorso corrisponde al tipo cercato: così non apriamo 90 pagine-immagine
    // per cercarci un video (spreco enorme di RAM/tempo). Le pagine "ambigue"
    // (nessun indizio) le apriamo comunque, per non perdere contenuti.
    let mut direct: Vec<&String> = Vec::new();
    let mut pages: Vec<&String> = Vec::new();
    for l in &all {
        if want_video && resolver::is_direct_video(l) {
            direct.push(l);
        } else if !want_video && resolver::is_direct_image(l) {
            direct.push(l);
        } else if resolver::is_web_page(l) {
            let hint_video = resolver::page_hints_video(l);
            let hint_image = resolver::page_hints_image(l);
            let matches = if want_video {
                hint_video || (!hint_image) // video, o ambigua
            } else {
                hint_image || (!hint_video) // immagine, o ambigua
            };
            if matches {
                pages.push(l);
            }
        }
    }

    let total = direct.len() + pages.len();
    if total == 0 {
        return false;
    }
    let kind = if want_video { "video" } else { "immagini" };
    emit(
        app,
        DlEvent::Line {
            index,
            line: format!("{total} {kind} aggiuntivi da elaborare..."),
        },
    );

    let mut saved = 0usize;
    let mut done = 0usize;
    let mut skipped = 0usize;

    // 1) File diretti: scaricati subito via HTTP, senza browser.
    for link in &direct {
        if inner.cancelled.load(Ordering::SeqCst) {
            break;
        }
        done += 1;
        emit(
            app,
            DlEvent::Progress {
                index,
                percent: (done as f64 / total as f64) * 100.0,
            },
        );
        // Deduplica: se questo URL è già stato scaricato in questa coda, lo salto
        if !first_time(inner, link) {
            skipped += 1;
            continue;
        }
        emit(
            app,
            DlEvent::Line {
                index,
                line: format!("Elemento {done}/{total}..."),
            },
        );
        if resolver::download_file(link, output_dir, "", Some(&inner.dedup)).await {
            saved += 1;
        } else {
            skipped += 1; // duplicato o errore
        }
    }

    // 2) Pagine: aperte col browser nascosto, a gruppi di 3 in parallelo.
    // 3 è il valore sicuro consigliato per non innescare i blocchi 429 (rate
    // limit), e tiene la memoria delle WebView2 sotto controllo.
    const PARALLEL: usize = 3;
    for group in pages.chunks(PARALLEL) {
        if inner.cancelled.load(Ordering::SeqCst) {
            break;
        }
        // Lancia le risoluzioni del gruppo insieme e aspetta che finiscano tutte.
        // La deduplica è sull'URL DIRETTO estratto (due pagine diverse potrebbero
        // puntare allo stesso file).
        let futures = group.iter().map(|link| async move {
            let url = resolver::resolve(app, link, 0, want).await?;
            if !first_time(inner, &url) {
                return Some(false); // stesso URL già scaricato
            }
            Some(resolver::download_file(&url, output_dir, "", Some(&inner.dedup)).await)
        });
        let results = futures_util::future::join_all(futures).await;

        for r in results {
            done += 1;
            emit(
                app,
                DlEvent::Progress {
                    index,
                    percent: (done as f64 / total as f64) * 100.0,
                },
            );
            match r {
                Some(true) => {
                    emit(
                        app,
                        DlEvent::Line {
                            index,
                            line: format!("Elemento {done}/{total}..."),
                        },
                    );
                    saved += 1;
                }
                Some(false) => skipped += 1, // duplicato o download fallito
                None => {}
            }
        }
    }

    if skipped > 0 {
        emit(
            app,
            DlEvent::Line {
                index,
                line: format!("{skipped} già presenti, saltati"),
            },
        );
    }

    emit(
        app,
        DlEvent::Line {
            index,
            line: format!("{saved}/{total} {kind} aggiuntivi scaricati"),
        },
    );
    saved > 0
}

/// Un elemento della coda: un link da scaricare con un motore specifico
type Task = (String, &'static str);

async fn run_queue(
    app: AppHandle,
    inner: Arc<Inner>,
    tasks: Vec<Task>,
    video_mode: String,
    cookies_browser: String,
    output_dir: String,
) {
    let total = tasks.len();
    let (mut ok, mut failed, mut nothing) = (0usize, 0usize, 0usize);

    emit(
        &app,
        DlEvent::QueueStart {
            tasks: tasks
                .iter()
                .map(|(url, engine)| TaskInfo {
                    url: url.clone(),
                    engine: engine.to_string(),
                })
                .collect(),
        },
    );

    for (i, (url, engine)) in tasks.iter().enumerate() {
        if inner.cancelled.load(Ordering::SeqCst) {
            break;
        }
        if i > 0 {
            // Attesa a piccoli passi, così Annulla risponde subito anche durante la pausa
            let mut left = jitter();
            while !left.is_zero() && !inner.cancelled.load(Ordering::SeqCst) {
                let step = left.min(Duration::from_millis(100));
                tokio::time::sleep(step).await;
                left -= step;
            }
            if inner.cancelled.load(Ordering::SeqCst) {
                break;
            }
        }
        emit(
            &app,
            DlEvent::ItemStart {
                index: i,
                total,
                url: url.clone(),
                engine: engine.to_string(),
            },
        );
        let outcome = run_one(
            &app,
            &inner,
            i,
            url,
            engine,
            &video_mode,
            &cookies_browser,
            &output_dir,
        )
        .await;
        emit(
            &app,
            DlEvent::ItemDone {
                index: i,
                outcome: outcome.as_str().into(),
            },
        );
        match outcome {
            Outcome::Ok => ok += 1,
            Outcome::Failed => failed += 1,
            Outcome::Nothing => nothing += 1,
        }
    }

    let cancelled = inner.cancelled.load(Ordering::SeqCst);
    emit(
        &app,
        DlEvent::Finished {
            ok,
            failed,
            nothing,
            cancelled,
        },
    );
    inner.cancelled.store(false, Ordering::SeqCst);
    inner.running.store(false, Ordering::SeqCst);
}

/// Avvia la coda di download. Usata sia dal comando IPC (finestra desktop)
/// sia dall'endpoint HTTP (telefono).
pub fn begin_download(
    app: &AppHandle,
    links: Vec<String>,
    video: bool,
    images: bool,
    video_mode: String,
    cookies_browser: String,
    output_dir: String,
) -> Result<(), String> {
    let state = app.state::<DownloadState>();

    if links.is_empty() {
        return Err("Nessun link ricevuto".into());
    }
    if !video && !images {
        return Err("Seleziona almeno un tipo di download (video o immagini)".into());
    }
    if !std::path::Path::new(&output_dir).is_dir() {
        return Err(format!("La cartella non esiste: {output_dir}"));
    }
    if state.0.running.swap(true, Ordering::SeqCst) {
        return Err("Un download è già in corso".into());
    }

    state.0.snapshot.lock().unwrap().last_output_dir = output_dir.clone();
    // Nuova coda: azzera i registri anti-duplicato (valgono per questa sessione)
    state.0.seen_urls.lock().unwrap().clear();
    state.0.dedup.clear();

    // Ogni link diventa un task per ciascun motore selezionato
    let mut tasks: Vec<Task> = Vec::new();
    for url in links {
        if video {
            tasks.push((url.clone(), "video"));
        }
        if images {
            tasks.push((url, "images"));
        }
    }

    let inner = state.0.clone();
    tauri::async_runtime::spawn(run_queue(
        app.clone(),
        inner,
        tasks,
        video_mode,
        cookies_browser,
        output_dir,
    ));
    Ok(())
}

/// Ferma la coda: uccide il processo corrente e scarta i task in attesa.
pub fn do_cancel(inner: &Inner) {
    inner.cancelled.store(true, Ordering::SeqCst);
    if let Some(child) = inner.current_child.lock().unwrap().take() {
        kill_tree(child);
    }
}

#[tauri::command]
async fn start_download(
    app: AppHandle,
    links: Vec<String>,
    video: bool,
    images: bool,
    video_mode: String,
    cookies_browser: String,
    output_dir: String,
) -> Result<String, String> {
    begin_download(
        &app,
        links,
        video,
        images,
        video_mode,
        cookies_browser,
        output_dir,
    )?;
    Ok("Avviato".into())
}

#[tauri::command]
fn cancel_download(state: State<'_, DownloadState>) -> Result<(), String> {
    do_cancel(&state.0);
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerInfo {
    port: Option<u16>,
    pin: String,
    addresses: Vec<String>,
}

/// Dati per il pannello "Accesso dal telefono" della finestra desktop
#[tauri::command]
fn server_info(state: State<'_, DownloadState>) -> ServerInfo {
    let port = *state.0.server_port.lock().unwrap();
    ServerInfo {
        port,
        pin: state.0.pin.clone(),
        addresses: port.map(server::local_addresses).unwrap_or_default(),
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    name: String,
    path: String,
}

/// Contenuto di una cartella, per il selettore usato dal telefono
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    /// None quando siamo alla radice (mostra solo le scorciatoie)
    path: Option<String>,
    parent: Option<String>,
    entries: Vec<DirEntry>,
    shortcuts: Vec<DirEntry>,
}

/// Cartelle note (Desktop, Download...) e dischi, come punti di partenza
fn shortcuts() -> Vec<DirEntry> {
    let mut out = Vec::new();
    if let Ok(home) = std::env::var("USERPROFILE") {
        for (label, sub) in [
            ("Desktop", "Desktop"),
            ("Download", "Downloads"),
            ("Video", "Videos"),
            ("Immagini", "Pictures"),
        ] {
            let p = std::path::Path::new(&home).join(sub);
            if p.is_dir() {
                out.push(DirEntry {
                    name: label.into(),
                    path: p.to_string_lossy().into_owned(),
                });
            }
        }
    }
    #[cfg(windows)]
    for c in b'A'..=b'Z' {
        let drive = format!("{}:\\", c as char);
        if std::path::Path::new(&drive).is_dir() {
            out.push(DirEntry {
                name: format!("Disco {}:", c as char),
                path: drive,
            });
        }
    }
    out
}

/// Elenca le sottocartelle di `path` (o le sole scorciatoie se assente).
pub fn list_dir(path: Option<String>) -> DirListing {
    let shortcuts = shortcuts();
    let Some(path) = path else {
        return DirListing {
            path: None,
            parent: None,
            entries: Vec::new(),
            shortcuts,
        };
    };

    let p = std::path::Path::new(&path);
    let mut entries = Vec::new();
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            if matches!(e.file_type(), Ok(ft) if ft.is_dir()) {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue; // cartelle nascoste
                }
                entries.push(DirEntry {
                    name,
                    path: e.path().to_string_lossy().into_owned(),
                });
            }
        }
    }
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let parent = p.parent().map(|pp| pp.to_string_lossy().into_owned());
    DirListing {
        path: Some(path),
        parent,
        entries,
        shortcuts,
    }
}

#[tauri::command]
fn browse_dir(path: Option<String>) -> DirListing {
    list_dir(path)
}

/// Crea una sottocartella dentro `parent` e restituisce il contenuto aggiornato.
pub fn make_dir(parent: &str, name: &str) -> Result<DirListing, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Il nome della cartella è vuoto".into());
    }
    // Evita che il nome contenga separatori o risalite di percorso
    if name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
        return Err("Il nome contiene caratteri non ammessi".into());
    }
    let target = std::path::Path::new(parent).join(name);
    std::fs::create_dir_all(&target).map_err(|e| format!("Impossibile creare la cartella: {e}"))?;
    Ok(list_dir(Some(parent.to_string())))
}

#[tauri::command]
fn create_dir(parent: String, name: String) -> Result<DirListing, String> {
    make_dir(&parent, &name)
}

/// Termina il processo e tutti i suoi figli. Necessario perché yt-dlp.exe
/// avvia un processo figlio che fa il vero download: uccidere solo il padre
/// lo lascerebbe attivo per diversi secondi.
fn kill_tree(child: CommandChild) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &child.pid().to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = child.kill();
    }
}

/// PIN a 6 cifre, generato al primo avvio e poi riletto dal file di config:
/// così il telefono lo chiede una volta sola.
fn load_or_create_pin(app: &AppHandle) -> String {
    let dir = app
        .path()
        .app_config_dir()
        .unwrap_or_else(|_| std::env::temp_dir());
    let file = dir.join("server-pin.txt");
    if let Ok(pin) = std::fs::read_to_string(&file) {
        let pin = pin.trim().to_string();
        if pin.len() == 6 && pin.chars().all(|c| c.is_ascii_digit()) {
            return pin;
        }
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pin = format!("{:06}", nanos % 1_000_000);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&file, &pin);
    pin
}

/// Vero se l'app è impostata per avviarsi con Windows
#[tauri::command]
fn autostart_enabled(app: AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Attiva/disattiva l'avvio automatico con Windows
#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let al = app.autolaunch();
    let res = if enabled { al.enable() } else { al.disable() };
    res.map_err(|e| e.to_string())
}

/// Nasconde la finestra tecnica "Tao Thread Event Target" (un residuo 16x16 a
/// (0,0) creato dal framework di finestre). È solo un target per gli eventi di
/// sistema, non deve essere visibile.
#[cfg(windows)]
fn hide_tao_event_window() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, ShowWindow, SW_HIDE};
    // Nome classe in UTF-16 con terminatore null
    let class: Vec<u16> = "Tao Thread Event Target\0".encode_utf16().collect();
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), std::ptr::null());
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
fn hide_tao_event_window() {}

/// Mostra la finestra principale (dal tray o alla seconda apertura dell'app),
/// riportandola al centro dello schermo se era parcheggiata fuori campo.
fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.center();
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Costruisce l'icona nel tray con menu Apri / Esci
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::{
        menu::{Menu, MenuItem},
        tray::TrayIconBuilder,
    };

    let open = MenuItem::with_id(app, "open", "Apri Harvest", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Esci", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Harvest — server attivo")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => {
                app.state::<DownloadState>()
                    .0
                    .quitting
                    .store(true, Ordering::SeqCst);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            use tauri::tray::{MouseButton, TrayIconEvent};
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .args(["--minimized"])
                .build(),
        )
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        // Alla seconda apertura dell'app riporto in primo piano quella già attiva
        .setup(|app| {
            let (tx, _) = tokio::sync::broadcast::channel(256);
            let state = DownloadState(Arc::new(Inner {
                running: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                current_child: Mutex::new(None),
                tx,
                snapshot: Mutex::new(Snapshot::default()),
                pin: load_or_create_pin(app.handle()),
                server_port: Mutex::new(None),
                quitting: AtomicBool::new(false),
                seen_urls: Mutex::new(std::collections::HashSet::new()),
                dedup: dedup::Dedup::default(),
            }));
            app.manage(state);
            server::start(app.handle().clone());
            build_tray(app.handle())?;

            // La finestra parte invisibile (config): la mostro subito, tranne
            // quando l'app è stata avviata da Windows con --minimized (resta nel tray).
            let minimized = std::env::args().any(|a| a == "--minimized");
            if let Some(w) = app.get_webview_window("main") {
                if minimized {
                    let _ = w.hide();
                } else {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }

            // Nasconde il residuo grafico 16x16 a (0,0): è la finestra tecnica
            // interna del framework ("Tao Thread Event Target"), non serve visibile.
            hide_tao_event_window();
            Ok(())
        })
        // La X nasconde nel tray invece di chiudere: il server resta attivo
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if !app
                    .state::<DownloadState>()
                    .0
                    .quitting
                    .load(Ordering::SeqCst)
                {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            start_download,
            cancel_download,
            server_info,
            browse_dir,
            create_dir,
            autostart_enabled,
            set_autostart
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
