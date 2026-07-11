// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod dedup;
mod engines;
mod resolver;
mod server;
mod store;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

#[derive(Clone, serde::Serialize)]
struct TaskInfo {
    url: String,
    engine: String,
}

/// Esito di un singolo task: distingue il fallimento vero dal caso
/// "questo motore non ha niente da scaricare qui" (es. gallery-dl su un video).
/// Il fallimento porta con sé un MOTIVO tradotto (mai un log grezzo), così la UI
/// può spiegare all'utente perché quel link non è stato scaricato.
#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Ok,
    Failed(FailReason),
    Nothing,
}

/// Causa di un fallimento, già in forma neutra (nessun host/URL/percorso).
/// Il testo mostrato all'utente è deciso da reason_message().
#[derive(Clone, Copy, PartialEq)]
enum FailReason {
    /// Il link richiede un login non disponibile
    LoginRequired,
    /// I cookie del browser non sono leggibili (Chromium blindato)
    CookiesUnreadable,
    /// Nessun formato/contenuto scaricabile trovato
    NoContent,
    /// Il motore è troppo vecchio per questo sito: va aggiornato
    EngineOutdated,
    /// Problema di rete o di avvio del motore
    Network,
    /// Annullato dall'utente
    Cancelled,
    /// Causa non identificata
    Unknown,
}

impl FailReason {
    /// Messaggio neutro e comprensibile per la UI (niente nomi di siti).
    fn message(self) -> &'static str {
        match self {
            FailReason::LoginRequired => "Needs a sign-in that isn't available",
            FailReason::CookiesUnreadable => {
                "Can't read the browser login — try Firefox, or no login"
            }
            FailReason::NoContent => "Nothing downloadable was found at this link",
            FailReason::EngineOutdated => {
                "The video engine is outdated — update it in Settings and try again"
            }
            FailReason::Network => "Network problem: try again later",
            FailReason::Cancelled => "Cancelled",
            FailReason::Unknown => "This link couldn't be downloaded",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            FailReason::LoginRequired => "loginRequired",
            FailReason::CookiesUnreadable => "cookiesUnreadable",
            FailReason::NoContent => "noContent",
            FailReason::EngineOutdated => "engineOutdated",
            FailReason::Network => "network",
            FailReason::Cancelled => "cancelled",
            FailReason::Unknown => "unknown",
        }
    }
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Failed(_) => "failed",
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
    /// Accoda nuovi task alla timeline esistente (es. "Riprova" di un elemento),
    /// invece di rimpiazzarla come QueueStart. base_index = da quale indice
    /// partono i nuovi task nella timeline.
    QueueAppend {
        #[serde(rename = "baseIndex")]
        base_index: usize,
        tasks: Vec<TaskInfo>,
    },
    ItemStart {
        index: usize,
        total: usize,
        url: String,
        engine: String,
    },
    /// Anteprima del contenuto (solo video, dove i metadati sono disponibili e la
    /// privacy lo consente): titolo, autore, durata, URL della miniatura.
    Preview {
        index: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        uploader: Option<String>,
        /// Durata in secondi
        #[serde(skip_serializing_if = "Option::is_none")]
        duration: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thumbnail: Option<String>,
    },
    /// Fase corrente dell'analisi (prima che parta il download vero), come stato
    /// testuale che evolve invece di un generico "Analizzo la pagina..." fermo.
    Phase {
        index: usize,
        phase: String,
    },
    Progress {
        index: usize,
        percent: f64,
        /// Velocità in byte/s (None se non nota)
        #[serde(skip_serializing_if = "Option::is_none")]
        speed: Option<f64>,
        /// Secondi rimanenti stimati (None se non noti)
        #[serde(skip_serializing_if = "Option::is_none")]
        eta: Option<f64>,
        /// Byte scaricati finora
        #[serde(skip_serializing_if = "Option::is_none")]
        downloaded: Option<u64>,
        /// Byte totali (o stima) del file
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
    },
    Line {
        index: usize,
        line: String,
    },
    ItemDone {
        index: usize,
        outcome: String,
        /// Motivo neutro del fallimento (solo quando outcome = "failed")
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Cartella di destinazione del task (per "Apri cartella")
        dir: String,
        /// Percorso del file prodotto, se unico (per "Mostra file").
        /// NB: rename esplicito, il rename_all dell'enum non tocca i campi.
        #[serde(rename = "filePath", skip_serializing_if = "Option::is_none")]
        file_path: Option<String>,
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
    /// Messaggio neutro del fallimento (solo quando status = "failed")
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// Anteprima (solo video): titolo, autore, durata, miniatura.
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uploader: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thumbnail: Option<String>,
}

impl SnapTask {
    /// Nuovo task "in attesa" a partire dalle info minime (url + engine).
    fn pending(t: &TaskInfo) -> Self {
        SnapTask {
            url: t.url.clone(),
            engine: t.engine.clone(),
            status: "pending".into(),
            reason: None,
            title: None,
            uploader: None,
            duration: None,
            thumbnail: None,
        }
    }
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
    /// Anti-brute-force del PIN: tentativi falliti recenti e istante di sblocco
    /// (secondi da UNIX_EPOCH). Vedi server::check_pin_rate_limit.
    pub pin_failures: Mutex<PinGuard>,
    /// Cartella di configurazione dell'app (PIN, cronologia, coda salvata)
    pub config_dir: std::path::PathBuf,
    /// Cronologia persistente dei download conclusi (history.json su disco)
    pub history: Mutex<Vec<store::HistoryEntry>>,
    /// Coda interrotta trovata all'avvio (queue.json rimasto su disco):
    /// alimenta il banner "Riprendi/Scarta". None dopo ripresa/scarto.
    pub interrupted: Mutex<Option<store::SavedQueue>>,
}

/// Stato dell'anti-brute-force del PIN (finestra scorrevole globale).
#[derive(Default)]
pub struct PinGuard {
    /// Tentativi falliti da quando è iniziata la finestra corrente
    pub fails: u32,
    /// Bloccato fino a questo istante (secondi da UNIX_EPOCH); 0 = non bloccato
    pub locked_until: u64,
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
                snap.timeline = tasks.iter().map(SnapTask::pending).collect();
            }
            DlEvent::QueueAppend { tasks, .. } => {
                snap.busy = true;
                snap.timeline
                    .extend(tasks.iter().map(SnapTask::pending));
            }
            DlEvent::ItemStart { index, .. } => {
                if let Some(t) = snap.timeline.get_mut(*index) {
                    t.status = "running".into();
                }
            }
            DlEvent::Preview {
                index,
                title,
                uploader,
                duration,
                thumbnail,
            } => {
                if let Some(t) = snap.timeline.get_mut(*index) {
                    t.title = title.clone();
                    t.uploader = uploader.clone();
                    t.duration = *duration;
                    t.thumbnail = thumbnail.clone();
                }
            }
            DlEvent::ItemDone {
                index,
                outcome,
                reason,
                ..
            } => {
                if let Some(t) = snap.timeline.get_mut(*index) {
                    t.status = outcome.clone();
                    t.reason = reason.clone();
                }
            }
            DlEvent::Finished { cancelled, .. } => {
                snap.busy = false;
                if *cancelled {
                    for t in &mut snap.timeline {
                        if t.status == "pending" || t.status == "running" {
                            t.status = "failed".into();
                            t.reason = Some(FailReason::Cancelled.message().into());
                        }
                    }
                }
            }
            _ => {}
        }
    }
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

/// Riga di progresso strutturato emessa da yt-dlp via --progress-template.
/// I campi mancanti arrivano come `null` (dopo aver sostituito il letterale NA).
#[derive(serde::Deserialize)]
struct ProgLine {
    downloaded: Option<u64>,
    total: Option<u64>,
    total_est: Option<u64>,
    speed: Option<f64>,
    eta: Option<f64>,
}

/// Traduce le righe di FASE di yt-dlp ("[youtube] Downloading webpage", ecc.) in
/// uno stato neutro e comprensibile che EVOLVE, così l'analisi non sembra ferma.
/// Riconosce pattern generici (non testi esatti, che cambiano tra versioni).
/// None per le righe che non sono fasi d'analisi.
fn analysis_phase(line: &str) -> Option<&'static str> {
    let l = line.to_lowercase();
    if l.contains("extracting url") {
        Some("Opening the page…")
    } else if l.contains("downloading webpage") {
        Some("Reading the page…")
    } else if l.contains("api json") || l.contains("player") || l.contains("client config") {
        Some("Querying the site…")
    } else if l.contains("m3u8") || l.contains("formats") || l.contains("format(s)") {
        Some("Checking available formats…")
    } else {
        None
    }
}

/// Host (dominio) di un URL, per decidere se serve la pausa anti-ban tra due
/// task: serve verso lo STESSO host, non tra host diversi. None se non parsabile.
fn host_of(url: &str) -> Option<String> {
    url.split("://")
        .nth(1)?
        .split(['/', '?', '#'])
        .next()
        .map(|h| h.trim_start_matches("www.").to_lowercase())
        .filter(|h| !h.is_empty())
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
        return Some("Skipped: one link needs a sign-in that isn't available".into());
    }
    None
}

/// Traduce le righe dei motori in stati NEUTRI da mostrare all'utente, senza mai
/// esporre nomi di host, URL o percorsi. Restituisce None per le righe da nascondere.
fn neutral_status(line: &str) -> Option<String> {
    let l = line.to_lowercase();
    if l.contains("error") && !l.contains("authorizationerror") {
        Some("One item couldn't be downloaded".into())
    } else if l.contains("[merger]") || l.contains("merging") {
        Some("Merging audio and video...".into())
    } else if l.contains("extracting") || l.contains("retrieving") {
        Some("Analysing the page...".into())
    } else {
        // Tutto il resto (percorsi di file salvati, info di debug) resta nascosto
        None
    }
}

/// Metadati d'anteprima estratti da yt-dlp (--print JSON), prima del download.
#[derive(serde::Deserialize)]
struct PreviewMeta {
    title: Option<String>,
    uploader: Option<String>,
    duration: Option<f64>,
    thumbnail: Option<String>,
}

/// Recupera in anticipo titolo, autore, durata e miniatura del video SENZA
/// scaricarlo, ed emette un evento Preview per la card. Best-effort: se fallisce
/// o ci mette troppo, il download parte comunque. Solo per i video (yt-dlp);
/// per le immagini dei forum non si fa, sia perché gallery-dl non dà questi
/// metadati sia per la regola privacy (niente titolo/provenienza dei forum).
async fn fetch_preview(app: &AppHandle, inner: &Inner, index: usize, url: &str) {
    let tmpl = r#"{"title":%(title)j,"uploader":%(uploader)j,"duration":%(duration)j,"thumbnail":%(thumbnail)j}"#;
    let cmd = match engines::ytdlp_command(app, &inner.config_dir) {
        Ok(c) => c.args(["--no-warnings", "--skip-download", "--print", tmpl, url]),
        Err(_) => return,
    };
    // Timeout prudente: l'anteprima non deve mai bloccare la coda.
    let fut = cmd.output();
    let out = match tokio::time::timeout(Duration::from_secs(12), fut).await {
        Ok(Ok(o)) => o,
        _ => return,
    };
    if inner.cancelled.load(Ordering::SeqCst) {
        return;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(line) = text.lines().find(|l| l.trim_start().starts_with('{')) else {
        return;
    };
    let json = line.replace(":NA", ":null");
    if let Ok(m) = serde_json::from_str::<PreviewMeta>(&json) {
        emit(
            app,
            DlEvent::Preview {
                index,
                title: m.title,
                uploader: m.uploader,
                duration: m.duration,
                thumbnail: m.thumbnail,
            },
        );
    }
}

/// Contatore per nomi univoci dei file temporanei di --print-to-file.
static FP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Legge il file scritto da yt-dlp con --print-to-file e lo elimina.
/// Restituisce il percorso solo se il task ha prodotto esattamente UN file
/// (una playlist ne scrive uno per riga: lì vale solo "Apri cartella").
fn take_single_filepath(fp: &Option<std::path::PathBuf>) -> Option<String> {
    let p = fp.as_deref()?;
    let text = std::fs::read_to_string(p).unwrap_or_default();
    let _ = std::fs::remove_file(p);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next()?.to_string();
    lines.next().is_none().then_some(first)
}

/// Scarica un singolo link con il sidecar giusto. Oltre all'esito restituisce
/// il percorso del file prodotto, quando è uno solo (per "Mostra file").
async fn run_one(
    app: &AppHandle,
    inner: &Inner,
    index: usize,
    url: &str,
    engine: &str,
    video_mode: &str,
    video_format: &str,
    max_height: u16,
    audio_format: &str,
    // Arricchimento: tag, copertina, capitoli nei file (default true dalla UI)
    enrich: bool,
    // Sottotitoli: "no" | "embed" (nel video) | "file" (.srt) | "both"
    subs: &str,
    concurrency: u8,
    cookies_browser: &str,
    output_dir: &str,
) -> (Outcome, Option<String>) {
    // Feedback immediato: appena parte il task mostriamo che stiamo iniziando,
    // prima ancora che il motore risponda (regola UX: reazione entro ~100ms).
    emit(
        app,
        DlEvent::Phase {
            index,
            phase: "Opening the page…".into(),
        },
    );
    // Anteprima (solo video): titolo, autore, durata, miniatura. Best-effort,
    // non blocca il download se fallisce.
    if engine == "video" {
        fetch_preview(app, inner, index, url).await;
        if inner.cancelled.load(Ordering::SeqCst) {
            return (Outcome::Failed(FailReason::Cancelled), None);
        }
    }

    // File temporaneo dove yt-dlp scrive il percorso ESATTO di ogni file
    // completato (per "Mostra file"). Assoluto: un percorso relativo verrebbe
    // risolto DENTRO -P. Se il percorso temp contiene '%' lo saltiamo: per
    // --print-to-file è sintassi di template e verrebbe reinterpretato.
    let fp_file = (engine == "video")
        .then(|| {
            let seq = FP_SEQ.fetch_add(1, Ordering::Relaxed);
            std::env::temp_dir().join(format!("harvest-fp-{}-{seq}.txt", std::process::id()))
        })
        .filter(|p| !p.to_string_lossy().contains('%'));

    let cookies = cookies_arg(cookies_browser);
    let (bin, args): (&str, Vec<String>) = if engine == "video" {
        let mut args = vec![
            "--newline".into(),
            "--no-warnings".into(),
            // Progresso in forma STRUTTURATA (una riga JSON per tick) invece del
            // testo umano: più affidabile da parsare e ci dà velocità, ETA e byte.
            // L'encoding `j` gestisce i valori mancanti come null senza rompere il JSON.
            "--progress-template".into(),
            r#"download:PROG {"downloaded":%(progress.downloaded_bytes)j,"total":%(progress.total_bytes)j,"total_est":%(progress.total_bytes_estimate)j,"speed":%(progress.speed)j,"eta":%(progress.eta)j}"#.into(),
            // Link video con playlist accodata (watch?v=...&list=...): scarica SOLO
            // il video incollato. I link playlist "puri" (/playlist?list=...)
            // scaricano comunque tutta la playlist.
            "--no-playlist".into(),
            // Frammenti dello stesso video scaricati in parallelo: su connessioni
            // veloci un singolo flusso non satura la linea, più flussi sì. Rischio
            // ban minimo (stesso video/CDN, non video diversi). 1 = com'era prima.
            "--concurrent-fragments".into(),
            concurrency.clamp(1, 16).to_string(),
            "-P".into(),
            output_dir.into(),
        ];
        // Tetto di risoluzione e formato (non riguarda il "solo audio").
        // -S ordina i formati e prende il migliore ENTRO i vincoli: non
        // fallisce mai se il valore esatto non esiste (verificato: res:480
        // sceglie 480p, vcodec:h264 sceglie avc1). Per "editing" il codec
        // H.264 ha priorità sulla risoluzione: è quello che DaVinci/Premiere
        // aprono senza problemi (su molti siti arriva fino a 1080p).
        if video_mode != "audioOnly" {
            let mut sort: Vec<String> = Vec::new();
            if video_format == "editing" {
                sort.push("vcodec:h264".into());
            }
            if max_height > 0 {
                sort.push(format!("res:{max_height}"));
            }
            if !sort.is_empty() {
                args.push("-S".into());
                args.push(sort.join(","));
            }
            // Solo il contenitore cambia (remux, nessuna ricodifica):
            // qualità identica, si apre ovunque.
            if video_format == "mp4" || video_format == "editing" {
                args.push("--remux-video".into());
                args.push("mp4".into());
            }
        }
        match video_mode {
            // Solo la traccia video, senza audio
            "videoOnly" => {
                args.push("-f".into());
                args.push("bv*".into());
            }
            // Solo l'audio, nel formato scelto dall'utente. "opus" = originale,
            // nessuna riconversione (qualità piena ma poco compatibile con gli
            // editor); mp3/wav riconvertiti da ffmpeg per l'uso in editing.
            "audioOnly" => {
                args.push("-x".into());
                match audio_format {
                    "mp3" => {
                        args.push("--audio-format".into());
                        args.push("mp3".into());
                        // VBR alla qualità più alta
                        args.push("--audio-quality".into());
                        args.push("0".into());
                    }
                    "wav" => {
                        args.push("--audio-format".into());
                        args.push("wav".into());
                    }
                    _ => {}
                }
            }
            // "full": video+audio, il default di yt-dlp (unisce con ffmpeg)
            _ => {}
        }
        // Arricchimento: metadati + copertina + capitoli nel file finale.
        // Un solo passaggio ffmpeg, nessuna ricodifica. Vale sia per l'audio
        // (l'MP3 esce con titolo/artista e copertina) sia per il video.
        // Il WAV non supporta copertine incorporate: --embed-thumbnail viene
        // semplicemente ignorato da yt-dlp per quel formato, senza errori.
        if enrich {
            args.push("--embed-metadata".into());
            args.push("--embed-chapters".into());
            // La COPERTINA solo dove il contenitore la gestisce nativamente:
            // audio (mp3) e video rimessi in MP4. In un video lasciato nel
            // formato originale (spesso webm/mkv) la copertina va allegata come
            // stream a parte, e per farlo yt-dlp invoca ffprobe — che NON è tra
            // i nostri sidecar: il download riuscirebbe ma il postprocessing
            // fallirebbe, marcando come "non riuscito" un file in realtà
            // completo (verificato). Meglio rinunciare alla sola copertina:
            // tag e capitoli restano, e la miniatura non era comunque
            // incorporabile in quel contenitore.
            let cover_ok = video_mode == "audioOnly"
                || video_format == "mp4"
                || video_format == "editing";
            if cover_ok {
                args.push("--embed-thumbnail".into());
            }
        }
        // Sottotitoli (solo quando c'è una traccia video: l'audio non li ha).
        // Lingua: inglese, codice ESATTO "en" (una sola traccia). yt-dlp non
        // espone in modo affidabile la "lingua originale" del video
        // (%(language)s è quasi sempre NA), quindi l'inglese è la scelta
        // prevedibile richiesta dall'utente.
        // ATTENZIONE (verificato dal vivo): un filtro con wildcard ("en.*")
        // scarica UNA traccia PER OGNI variante YouTube (en, en-en, en-de...):
        // alla terza scatta HTTP 429 Too Many Requests, che con il
        // comportamento di default fa fallire l'INTERO download, video
        // compreso. Il codice esatto "en" ne prende una sola: niente 429.
        // --sub-format srt richiede il formato giusto già dalla fonte;
        // --convert-subs srt garantisce l'srt anche se arriva solo il vtt (il
        // formato che gli editor importano). "embed" = dentro al video (MP4/MKV
        // sì, WEBM no), "file" = .srt accanto, "both" = entrambi.
        // --write-auto-subs recupera gli auto-generati quando non c'è la
        // traccia manuale. Se il sottotitolo non c'è, il video si scarica
        // comunque (non è un errore fatale).
        if video_mode != "audioOnly" && subs != "no" {
            args.push("--sub-langs".into());
            args.push("en".into());
            args.push("--write-auto-subs".into());
            args.push("--sub-format".into());
            args.push("srt/best".into());
            args.push("--convert-subs".into());
            args.push("srt".into());
            if subs == "file" || subs == "both" {
                args.push("--write-subs".into());
            }
            if subs == "embed" || subs == "both" {
                // Con "embed" senza --write-subs, yt-dlp scarica il sottotitolo,
                // lo incorpora e cancella il temporaneo da sé: nessun .srt resta.
                args.push("--embed-subs".into());
            }
        }
        // NB: NIENTE cookie per i video. Su YouTube i cookie di un account loggato
        // fanno scattare controlli anti-bot che bloccano il download ("No video
        // formats found"). Il login serve solo alle immagini (gallery-dl).
        if let Some(dir) = ffmpeg_dir() {
            args.push("--ffmpeg-location".into());
            args.push(dir);
        }
        // Percorso di ogni file completato, una riga per file, scritto DOPO lo
        // spostamento finale. --print-to-file accende la modalità quiet:
        // --no-quiet la rispegne, altrimenti perderemmo fasi e progresso.
        if let Some(fp) = &fp_file {
            args.push("--print-to-file".into());
            args.push("after_move:filepath".into());
            args.push(fp.to_string_lossy().into_owned());
            args.push("--no-quiet".into());
        }
        args.push(url.into());
        ("yt-dlp", args)
    } else {
        // --sleep: pausa casuale tra una richiesta e l'altra dentro la stessa
        // galleria, per non farsi bloccare con 429 Too Many Requests.
        // -R 4: fino a 4 tentativi per errori TRANSITORI, così non si perdono
        // file recuperabili. gallery-dl ritenta di default solo 5xx / 429 / errori
        // di rete; i 4xx come login (401/403) NON vengono ritentati, quindi i link
        // che richiedono un accesso vengono comunque scartati subito (com'era con
        // -R 1). --sleep-429: attesa dedicata quando si riceve un 429 (rate limit).
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
            "4".into(),
            "--sleep-429".into(),
            "10.0".into(),
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

    // yt-dlp può essere la copia aggiornata (vedi engines.rs); gallery-dl no.
    let launcher = if bin == "yt-dlp" {
        engines::ytdlp_command(app, &inner.config_dir)
    } else {
        app.shell().sidecar(bin).map_err(|e| e.to_string())
    };
    let cmd = match launcher {
        Ok(c) => c.args(args),
        Err(e) => {
            emit(
                app,
                DlEvent::Line {
                    index,
                    line: format!("Engine error {bin}: {e}"),
                },
            );
            return (Outcome::Failed(FailReason::Network), None);
        }
    };

    let (mut rx, child) = match cmd.spawn() {
        Ok(pair) => pair,
        Err(e) => {
            emit(
                app,
                DlEvent::Line {
                    index,
                    line: format!("Couldn't start {bin}: {e}"),
                },
            );
            return (Outcome::Failed(FailReason::Network), None);
        }
    };
    *inner.current_child.lock().unwrap() = Some(child);

    let mut exit_code: Option<i32> = None;
    let mut unsupported = false;
    // Causa più specifica intercettata dall'output: se resta None a fine task
    // fallito, si ripiega su un motivo generico. Vedi FailReason.
    let mut fail_reason: Option<FailReason> = None;
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
                // Nessun formato/contenuto scaricabile (yt-dlp)
                if line.contains("No video formats found")
                    || line.contains("Requested format is not available")
                {
                    fail_reason = Some(FailReason::NoContent);
                }
                // Motore troppo vecchio: è il testo che yt-dlp stampa quando un
                // estrattore si rompe (tipico dopo un cambiamento lato sito).
                // Sovrascrive le cause più generiche: qui il rimedio è preciso.
                if line.contains("Confirm you are on the latest version")
                    || line.contains("Please update to the latest version")
                {
                    fail_reason = Some(FailReason::EngineOutdated);
                }
                // Progresso strutturato di yt-dlp: "PROG {json}". Ne ricaviamo
                // percentuale, velocità, ETA e byte per la UI.
                if let Some(json) = line.strip_prefix("PROG ") {
                    // yt-dlp scrive il letterale NA per i valori mancanti: lo
                    // rendiamo null così il JSON è valido.
                    let json = json.replace(":NA", ":null");
                    if let Ok(p) = serde_json::from_str::<ProgLine>(&json) {
                        let total = p.total.or(p.total_est);
                        let percent = match (p.downloaded, total) {
                            (Some(d), Some(t)) if t > 0 => (d as f64 / t as f64) * 100.0,
                            _ => 0.0,
                        };
                        emit(
                            app,
                            DlEvent::Progress {
                                index,
                                percent,
                                speed: p.speed,
                                eta: p.eta,
                                downloaded: p.downloaded,
                                total,
                            },
                        );
                    }
                    continue;
                }
                // Fase di analisi (prima del download): stato che evolve.
                if let Some(phase) = analysis_phase(&line) {
                    emit(
                        app,
                        DlEvent::Phase {
                            index,
                            phase: phase.into(),
                        },
                    );
                    continue;
                }
                // Errore noto dei cookie Chromium: lo traduco in un messaggio utile
                if line.contains("DPAPI")
                    || line.contains("could not find") && line.contains("cookies")
                {
                    fail_reason = Some(FailReason::CookiesUnreadable);
                    emit(app, DlEvent::Line {
                        index,
                        line: "Browser cookies can't be read (Chrome/Edge/Brave protection). Try Firefox, or no login.".into(),
                    });
                    continue;
                }
                // Link che richiede un login non disponibile: messaggio neutro.
                if let Some(msg) = skipped_third_party(&line) {
                    fail_reason = Some(FailReason::LoginRequired);
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

    // Percorso del file prodotto (e pulizia del temporaneo), da leggere
    // comunque: anche su annullo un file può essere stato completato.
    let file_path = take_single_filepath(&fp_file);

    if inner.cancelled.load(Ordering::SeqCst) {
        return (Outcome::Failed(FailReason::Cancelled), file_path);
    }

    // Il motore ha scaricato il link: lo registro come già preso, così il
    // resolver (che parte qui sotto) non lo riscarica una seconda volta. Capita
    // sui link a un file diretto — es. un .mp4 incollato — che sia yt-dlp sia il
    // resolver sanno gestire: senza questo, lo stesso file veniva scaricato due
    // volte (doppio tempo e doppio traffico).
    if exit_code == Some(0) {
        first_time(inner, url);
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
        return (Outcome::Ok, file_path);
    }
    // gallery-dl usa un codice a bit: il bit 64 significa "nessun estrattore
    // per questo URL", cioè niente da scaricare per questo motore
    if unsupported || (bin == "gallery-dl" && exit_code.is_some_and(|c| c & 64 != 0)) {
        // Se comunque abbiamo preso media extra, il task ha prodotto qualcosa
        // (file scaricati dal resolver: più d'uno possibile, niente percorso singolo)
        return if extra_ok {
            (Outcome::Ok, file_path)
        } else {
            (Outcome::Nothing, None)
        };
    }
    if extra_ok {
        return (Outcome::Ok, file_path);
    }
    (Outcome::Failed(fail_reason.unwrap_or(FailReason::Unknown)), None)
}

/// Registra un URL come "scaricato" e dice se è la PRIMA volta che lo si vede in
/// questa coda. La chiave è calcolata da resolver::dedup_key: ignora la query
/// per i file con estensione nota (token variabili) ma la mantiene per gli URL
/// senza estensione (dove la query identifica il file). Vedi resolver::dedup_key.
fn first_time(inner: &Inner, url: &str) -> bool {
    inner
        .seen_urls
        .lock()
        .unwrap()
        .insert(resolver::dedup_key(url))
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
    let kind = if want_video { "videos" } else { "images" };
    emit(
        app,
        DlEvent::Line {
            index,
            line: format!("{total} extra {kind} to process..."),
        },
    );

    let mut saved = 0usize;
    let mut done = 0usize;
    let mut skipped = 0usize;

    // Client HTTP condiviso: riusa le connessioni (keep-alive) tra tutti i file
    // invece di rifare l'handshake TLS a ogni download.
    let client = resolver::http_client();

    // Gruppi paralleli, con due limiti diversi:
    // - file diretti: semplici GET HTTP, leggeri → 4 in parallelo (poco rischio 429).
    // - pagine WebView: ognuna apre un processo browser → 3, per tenere RAM e
    //   rate-limit sotto controllo (valore sicuro consigliato).
    const PARALLEL_FILES: usize = 4;
    const PARALLEL_PAGES: usize = 3;

    // 1) File diretti: scaricati via HTTP, senza browser, a gruppi in parallelo.
    for group in direct.chunks(PARALLEL_FILES) {
        if inner.cancelled.load(Ordering::SeqCst) {
            break;
        }
        let futures = group.iter().map(|link| {
            let client = &client;
            async move {
                // Dedup sull'URL: se già scaricato in questa coda, salta.
                if !first_time(inner, link) {
                    return Some(false);
                }
                Some(resolver::download_file(client, link, output_dir, "", Some(&inner.dedup)).await)
            }
        });
        let results = futures_util::future::join_all(futures).await;

        for r in results {
            done += 1;
            emit(
                app,
                DlEvent::Progress {
                    index,
                    percent: (done as f64 / total as f64) * 100.0,
                    speed: None,
                    eta: None,
                    downloaded: None,
                    total: None,
                },
            );
            match r {
                Some(true) => {
                    emit(
                        app,
                        DlEvent::Line {
                            index,
                            line: format!("Item {done}/{total}..."),
                        },
                    );
                    saved += 1;
                }
                _ => skipped += 1, // duplicato o errore
            }
        }
    }

    // 2) Pagine: aperte col browser nascosto, a gruppi in parallelo.
    for group in pages.chunks(PARALLEL_PAGES) {
        if inner.cancelled.load(Ordering::SeqCst) {
            break;
        }
        // Lancia le risoluzioni del gruppo insieme e aspetta che finiscano tutte.
        // La deduplica è sull'URL DIRETTO estratto (due pagine diverse potrebbero
        // puntare allo stesso file).
        let futures = group.iter().map(|link| {
            let client = &client;
            async move {
                let url = resolver::resolve(app, link, 0, want).await?;
                if !first_time(inner, &url) {
                    return Some(false); // stesso URL già scaricato
                }
                Some(resolver::download_file(client, &url, output_dir, "", Some(&inner.dedup)).await)
            }
        });
        let results = futures_util::future::join_all(futures).await;

        for r in results {
            done += 1;
            emit(
                app,
                DlEvent::Progress {
                    index,
                    percent: (done as f64 / total as f64) * 100.0,
                    speed: None,
                    eta: None,
                    downloaded: None,
                    total: None,
                },
            );
            match r {
                Some(true) => {
                    emit(
                        app,
                        DlEvent::Line {
                            index,
                            line: format!("Item {done}/{total}..."),
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
                line: format!("{skipped} already there, skipped"),
            },
        );
    }

    emit(
        app,
        DlEvent::Line {
            index,
            line: format!("{saved}/{total} extra {kind} downloaded"),
        },
    );
    saved > 0
}

/// Un elemento della coda: un link da scaricare con un motore specifico
type Task = (String, &'static str);

#[allow(clippy::too_many_arguments)]
async fn run_queue(
    app: AppHandle,
    inner: Arc<Inner>,
    tasks: Vec<Task>,
    base_index: usize,
    video_mode: String,
    video_format: String,
    max_height: u16,
    audio_format: String,
    enrich: bool,
    subs: String,
    concurrency: u8,
    cookies_browser: String,
    output_dir: String,
) {
    let total = base_index + tasks.len();
    let (mut ok, mut failed, mut nothing) = (0usize, 0usize, 0usize);

    let infos: Vec<TaskInfo> = tasks
        .iter()
        .map(|(url, engine)| TaskInfo {
            url: url.clone(),
            engine: engine.to_string(),
        })
        .collect();
    // Prima coda → QueueStart (rimpiazza); Riprova → QueueAppend (accoda).
    emit(
        &app,
        if base_index == 0 {
            DlEvent::QueueStart { tasks: infos }
        } else {
            DlEvent::QueueAppend {
                base_index,
                tasks: infos,
            }
        },
    );

    // Host dell'ultimo task avviato: la pausa anti-ban serve solo tra richieste
    // allo STESSO host, non tra host diversi (che non condividono il rate limit).
    let mut last_host: Option<String> = None;
    for (offset, (url, engine)) in tasks.iter().enumerate() {
        // Indice ASSOLUTO nella timeline (con Riprova i task partono da base_index)
        let i = base_index + offset;
        if inner.cancelled.load(Ordering::SeqCst) {
            break;
        }
        let this_host = host_of(url);
        // Pausa se il task precedente era verso lo stesso host. Nel dubbio (host
        // non determinabile) la teniamo, per non togliere una protezione.
        let same_host = this_host.is_none() || last_host.is_none() || this_host == last_host;
        if offset > 0 && same_host {
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
        last_host = this_host;
        emit(
            &app,
            DlEvent::ItemStart {
                index: i,
                total,
                url: url.clone(),
                engine: engine.to_string(),
            },
        );
        let (outcome, file_path) = run_one(
            &app,
            &inner,
            i,
            url,
            engine,
            &video_mode,
            &video_format,
            max_height,
            &audio_format,
            enrich,
            &subs,
            concurrency,
            &cookies_browser,
            &output_dir,
        )
        .await;
        let reason = match outcome {
            Outcome::Failed(r) => Some(r.message().to_string()),
            _ => None,
        };
        emit(
            &app,
            DlEvent::ItemDone {
                index: i,
                outcome: outcome.as_str().into(),
                reason: reason.clone(),
                dir: output_dir.clone(),
                file_path: file_path.clone(),
            },
        );
        // Voce di cronologia: porta con sé l'anteprima già raccolta dalla
        // fotografia (solo video; per i forum resta tutto anonimo).
        let (title, uploader, duration, thumbnail) = {
            let snap = inner.snapshot.lock().unwrap();
            snap.timeline
                .get(i)
                .map(|t| {
                    (
                        t.title.clone(),
                        t.uploader.clone(),
                        t.duration,
                        t.thumbnail.clone(),
                    )
                })
                .unwrap_or_default()
        };
        push_history(
            &inner,
            store::HistoryEntry {
                url: url.clone(),
                engine: engine.to_string(),
                outcome: outcome.as_str().into(),
                reason,
                when: store::now_secs(),
                dir: output_dir.clone(),
                file_path,
                title,
                uploader,
                duration,
                thumbnail,
            },
        );
        // Aggiorna la coda su disco: restano solo i task non ancora fatti.
        // Se l'app muore da qui in poi, al riavvio si riparte da questi.
        store::save_queue(
            &inner.config_dir,
            &store::SavedQueue {
                tasks: tasks[offset + 1..]
                    .iter()
                    .map(|(u, e)| store::SavedTask {
                        url: u.clone(),
                        engine: (*e).to_string(),
                    })
                    .collect(),
                video_mode: video_mode.clone(),
                video_format: video_format.clone(),
                max_height,
                audio_format: audio_format.clone(),
                enrich,
                subs: subs.clone(),
                concurrency,
                cookies_browser: cookies_browser.clone(),
                output_dir: output_dir.clone(),
            },
        );
        match outcome {
            Outcome::Ok => ok += 1,
            Outcome::Failed(_) => failed += 1,
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
    // Coda conclusa (o annullata dall'utente): niente da riprendere al riavvio
    store::delete_queue(&inner.config_dir);
    inner.cancelled.store(false, Ordering::SeqCst);
    inner.running.store(false, Ordering::SeqCst);
}

/// Aggiunge una voce alla cronologia e la salva su disco, scartando le voci
/// più vecchie oltre il tetto (le nuove stanno in fondo al file).
fn push_history(inner: &Inner, entry: store::HistoryEntry) {
    let mut h = inner.history.lock().unwrap();
    h.push(entry);
    if h.len() > store::HISTORY_MAX {
        let overflow = h.len() - store::HISTORY_MAX;
        h.drain(..overflow);
    }
    store::save_history(&inner.config_dir, &h);
}

/// Avvia la coda di download. Usata sia dal comando IPC (finestra desktop)
/// sia dall'endpoint HTTP (telefono).
#[allow(clippy::too_many_arguments)]
pub fn begin_download(
    app: &AppHandle,
    links: Vec<String>,
    video: bool,
    images: bool,
    video_mode: String,
    video_format: String,
    max_height: u16,
    audio_format: String,
    enrich: bool,
    subs: String,
    concurrency: u8,
    cookies_browser: String,
    output_dir: String,
    // true = accoda alla timeline esistente (Riprova); false = nuova coda.
    append: bool,
) -> Result<(), String> {
    if links.is_empty() {
        return Err("No links received".into());
    }
    if !video && !images {
        return Err("Pick at least one download type (video or images)".into());
    }
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

    spawn_queue(
        app,
        tasks,
        video_mode,
        video_format,
        max_height,
        audio_format,
        enrich,
        subs,
        concurrency,
        cookies_browser,
        output_dir,
        append,
    )
}

/// Prepara lo stato e lancia la coda. Condivisa tra begin_download (coda
/// nuova o Riprova) e resume_queue_inner (ripresa di una coda interrotta).
#[allow(clippy::too_many_arguments)]
fn spawn_queue(
    app: &AppHandle,
    tasks: Vec<Task>,
    video_mode: String,
    video_format: String,
    max_height: u16,
    audio_format: String,
    enrich: bool,
    subs: String,
    concurrency: u8,
    cookies_browser: String,
    output_dir: String,
    append: bool,
) -> Result<(), String> {
    let state = app.state::<DownloadState>();

    if !std::path::Path::new(&output_dir).is_dir() {
        return Err(format!("Folder doesn't exist: {output_dir}"));
    }
    if state.0.running.swap(true, Ordering::SeqCst) {
        return Err("A download is already running".into());
    }

    state.0.snapshot.lock().unwrap().last_output_dir = output_dir.clone();
    // Il registro degli URL già presi si azzera SEMPRE, anche con "Riprova":
    // altrimenti il link che sto ritentando risulterebbe "già scaricato" e
    // verrebbe saltato subito, rendendo il bottone inutile.
    state.0.seen_urls.lock().unwrap().clear();
    if !append {
        // Nuova coda: azzero anche le impronte dei contenuti. Con "Riprova"
        // invece le tengo: i file già scaricati restano riconoscibili come
        // doppioni, così ritentare un elemento non ne crea una seconda copia.
        state.0.dedup.clear();
    }
    let base_index = if append {
        state.0.snapshot.lock().unwrap().timeline.len()
    } else {
        0
    };

    // Una coda nuova rende obsoleta l'eventuale coda interrotta (il suo file
    // su disco viene comunque sovrascritto qui sotto): via il banner.
    *state.0.interrupted.lock().unwrap() = None;
    // Fotografia iniziale della coda su disco: se l'app muore prima della
    // fine, al riavvio questi task compaiono nel banner Riprendi/Scarta.
    store::save_queue(
        &state.0.config_dir,
        &store::SavedQueue {
            tasks: tasks
                .iter()
                .map(|(u, e)| store::SavedTask {
                    url: u.clone(),
                    engine: (*e).to_string(),
                })
                .collect(),
            video_mode: video_mode.clone(),
            video_format: video_format.clone(),
            max_height,
            audio_format: audio_format.clone(),
            enrich,
            subs: subs.clone(),
            concurrency,
            cookies_browser: cookies_browser.clone(),
            output_dir: output_dir.clone(),
        },
    );

    let inner = state.0.clone();
    tauri::async_runtime::spawn(run_queue(
        app.clone(),
        inner,
        tasks,
        base_index,
        video_mode,
        video_format,
        max_height,
        audio_format,
        enrich,
        subs,
        concurrency,
        cookies_browser,
        output_dir,
    ));
    Ok(())
}

/// Riprende la coda interrotta trovata all'avvio, con gli stessi parametri
/// della sessione precedente. Condivisa tra IPC e endpoint HTTP.
pub fn resume_queue_inner(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<DownloadState>();
    let saved = state
        .0
        .interrupted
        .lock()
        .unwrap()
        .take()
        .ok_or("Nothing to resume")?;

    // Gli engine su disco tornano ai valori statici usati dalla coda;
    // eventuali valori sconosciuti (file manomesso) vengono scartati.
    let tasks: Vec<Task> = saved
        .tasks
        .iter()
        .filter_map(|t| match t.engine.as_str() {
            "video" => Some((t.url.clone(), "video")),
            "images" => Some((t.url.clone(), "images")),
            _ => None,
        })
        .collect();
    if tasks.is_empty() {
        store::delete_queue(&state.0.config_dir);
        return Err("Nothing to resume".into());
    }

    spawn_queue(
        app,
        tasks,
        saved.video_mode,
        saved.video_format,
        saved.max_height,
        saved.audio_format,
        saved.enrich,
        saved.subs,
        saved.concurrency,
        saved.cookies_browser,
        saved.output_dir,
        false,
    )
    .inspect_err(|_| {
        // Ripresa fallita (es. cartella sparita): il file su disco resta,
        // così l'utente può riprovare o scartare esplicitamente.
        *app.state::<DownloadState>().0.interrupted.lock().unwrap() =
            store::load_queue(&app.state::<DownloadState>().0.config_dir);
    })
}

/// Scarta la coda interrotta: via il file su disco e il banner.
pub fn discard_queue_inner(inner: &Inner) {
    *inner.interrupted.lock().unwrap() = None;
    store::delete_queue(&inner.config_dir);
}

/// Ferma la coda: uccide il processo corrente e scarta i task in attesa.
pub fn do_cancel(inner: &Inner) {
    inner.cancelled.store(true, Ordering::SeqCst);
    if let Some(child) = inner.current_child.lock().unwrap().take() {
        kill_tree(child);
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn start_download(
    app: AppHandle,
    links: Vec<String>,
    video: bool,
    images: bool,
    video_mode: String,
    video_format: String,
    max_height: u16,
    audio_format: String,
    enrich: bool,
    subs: String,
    concurrency: u8,
    cookies_browser: String,
    output_dir: String,
    #[allow(non_snake_case)] append: Option<bool>,
) -> Result<String, String> {
    begin_download(
        &app,
        links,
        video,
        images,
        video_mode,
        video_format,
        max_height,
        audio_format,
        enrich,
        subs,
        concurrency,
        cookies_browser,
        output_dir,
        append.unwrap_or(false),
    )?;
    Ok("Started".into())
}

#[tauri::command]
fn cancel_download(state: State<'_, DownloadState>) -> Result<(), String> {
    do_cancel(&state.0);
    Ok(())
}

/// Rimuove un elemento dalla timeline (solo a coda ferma). Condivisa tra il
/// comando IPC (desktop) e l'endpoint HTTP (telefono).
pub fn remove_item_inner(inner: &Inner, index: usize) -> Result<(), String> {
    if inner.running.load(Ordering::SeqCst) {
        return Err("Can't remove while downloading".into());
    }
    let mut snap = inner.snapshot.lock().unwrap();
    if index < snap.timeline.len() {
        snap.timeline.remove(index);
    }
    Ok(())
}

#[tauri::command]
fn remove_item(state: State<'_, DownloadState>, index: usize) -> Result<(), String> {
    remove_item_inner(&state.0, index)
}

/// Cronologia completa (dalla più vecchia alla più recente; la UI la inverte)
#[tauri::command]
fn get_history(state: State<'_, DownloadState>) -> Vec<store::HistoryEntry> {
    state.0.history.lock().unwrap().clone()
}

#[tauri::command]
fn clear_history(state: State<'_, DownloadState>) {
    let mut h = state.0.history.lock().unwrap();
    h.clear();
    store::save_history(&state.0.config_dir, &h);
}

/// Coda interrotta trovata all'avvio (per il banner Riprendi/Scarta)
#[tauri::command]
fn interrupted_queue(state: State<'_, DownloadState>) -> Option<store::SavedQueue> {
    state.0.interrupted.lock().unwrap().clone()
}

/// Motori mancanti (es. messi in quarantena da un antivirus). Vuoto = tutto ok.
/// Controllo istantaneo, fatto all'avvio: senza, l'app sembrerebbe a posto e
/// fallirebbe solo al primo download con un errore generico.
#[tauri::command]
fn missing_engines(state: State<'_, DownloadState>) -> Vec<String> {
    engines::missing_engines(&state.0.config_dir)
}

/// Versione del motore video e, se c'è, quella disponibile.
/// Non scarica nulla: alimenta la pillola di aggiornamento nell'header.
#[tauri::command]
async fn check_engine(app: AppHandle, state: State<'_, DownloadState>) -> Result<engines::EngineInfo, String> {
    let dir = state.0.config_dir.clone();
    Ok(engines::check(&app, &dir).await)
}

/// Aggiorna il motore video all'ultima versione. Restituisce la nuova versione.
#[tauri::command]
async fn update_engine(app: AppHandle, state: State<'_, DownloadState>) -> Result<String, String> {
    let dir = state.0.config_dir.clone();
    engines::update(&app, &dir).await
}

#[tauri::command]
fn resume_queue(app: AppHandle) -> Result<(), String> {
    resume_queue_inner(&app)
}

#[tauri::command]
fn discard_queue(state: State<'_, DownloadState>) {
    discard_queue_inner(&state.0);
}

/// Apre la cartella in Esplora risorse ("Apri cartella")
#[tauri::command]
fn open_folder(app: AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

/// Apre Esplora risorse con il file già selezionato ("Mostra file")
#[tauri::command]
fn reveal_file(app: AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|e| e.to_string())
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
            ("Downloads", "Downloads"),
            ("Videos", "Videos"),
            ("Pictures", "Pictures"),
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
                name: format!("Drive {}:", c as char),
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
        return Err("The folder name is empty".into());
    }
    // Evita che il nome contenga separatori o risalite di percorso
    if name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
        return Err("The name contains characters that aren't allowed".into());
    }
    let target = std::path::Path::new(parent).join(name);
    std::fs::create_dir_all(&target).map_err(|e| format!("Couldn't create the folder: {e}"))?;
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
fn load_or_create_pin(dir: &std::path::Path) -> String {
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

    let open = MenuItem::with_id(app, "open", "Open Harvest", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Harvest — server running")
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
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        // Alla seconda apertura dell'app riporto in primo piano quella già attiva
        .setup(|app| {
            let (tx, _) = tokio::sync::broadcast::channel(256);
            // Cartella di configurazione: PIN, cronologia e coda salvata
            let config_dir = app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::env::temp_dir());
            let state = DownloadState(Arc::new(Inner {
                running: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                current_child: Mutex::new(None),
                tx,
                snapshot: Mutex::new(Snapshot::default()),
                pin: load_or_create_pin(&config_dir),
                server_port: Mutex::new(None),
                quitting: AtomicBool::new(false),
                seen_urls: Mutex::new(std::collections::HashSet::new()),
                dedup: dedup::Dedup::default(),
                pin_failures: Mutex::new(PinGuard::default()),
                history: Mutex::new(store::load_history(&config_dir)),
                // Un queue.json presente all'avvio = sessione precedente
                // interrotta a metà coda: proponi Riprendi/Scarta.
                interrupted: Mutex::new(store::load_queue(&config_dir)),
                config_dir,
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
            remove_item,
            get_history,
            clear_history,
            interrupted_queue,
            resume_queue,
            discard_queue,
            check_engine,
            update_engine,
            missing_engines,
            open_folder,
            reveal_file,
            server_info,
            browse_dir,
            create_dir,
            autostart_enabled,
            set_autostart
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
