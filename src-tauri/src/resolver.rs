//! Risolutore per host dove l'immagine reale è caricata via JavaScript e non è
//! raggiungibile né da gallery-dl né da una GET semplice.
//! Apre una WebView fuori schermo, lascia girare il JS, ne estrae l'URL diretto.
//!
//! Il risultato torna a Rust tramite l'HASH dell'URL (location.hash), letto con
//! window.url(): non serve l'IPC, che Tauri blocca sui domini remoti — quindi
//! nessun dominio esterno va autorizzato ad accedere all'app.

use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use tauri::{AppHandle, LogicalPosition, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_shell::ShellExt;

/// Candidati alla risoluzione via browser: pagine web (non file diretti) che
/// gallery-dl elenca ma non scarica da solo. Il resolver le apre e tiene solo
/// quelle da cui estrae davvero un'immagine — così NON serve nessuna lista di
/// nomi di siti: la decisione è per comportamento, non per dominio.
const VIDEO_EXT: &[&str] = &[".mp4", ".webm", ".mov", ".m4v", ".mkv", ".avi"];
const IMAGE_EXT: &[&str] = &[
    ".jpg", ".jpeg", ".png", ".webp", ".gif", ".bmp", ".tiff", ".avif",
];

/// Vero se l'URL è una PAGINA web (non un file diretto): va aperta col browser.
pub fn is_web_page(url: &str) -> bool {
    let lower = url.split('?').next().unwrap_or("").to_lowercase();
    let is_direct = VIDEO_EXT
        .iter()
        .chain(IMAGE_EXT.iter())
        .any(|e| lower.ends_with(e))
        || lower.ends_with(".mp3");
    !is_direct && lower.starts_with("http")
}

/// Indizio dal percorso dell'URL su cosa contiene una pagina-embed, così NON
/// apriamo pagine-immagine quando cerchiamo video e viceversa (pattern generici,
/// usati da moltissimi host: /embed|/watch|/video → video; /img|/image → immagine).
pub fn page_hints_video(url: &str) -> bool {
    let l = url.to_lowercase();
    ["/embed", "/watch", "/video", "/v/", "/e/"]
        .iter()
        .any(|p| l.contains(p))
}

pub fn page_hints_image(url: &str) -> bool {
    let l = url.to_lowercase();
    ["/img", "/image", "/i/", "/photo", "/pic"]
        .iter()
        .any(|p| l.contains(p))
}

/// Vero se l'URL è un file VIDEO diretto (già scaricabile senza browser).
pub fn is_direct_video(url: &str) -> bool {
    let lower = url.split('?').next().unwrap_or("").to_lowercase();
    VIDEO_EXT.iter().any(|e| lower.ends_with(e))
}

/// Vero se l'URL è un file IMMAGINE diretto.
pub fn is_direct_image(url: &str) -> bool {
    let lower = url.split('?').next().unwrap_or("").to_lowercase();
    IMAGE_EXT.iter().any(|e| lower.ends_with(e))
}

/// Chiave anti-duplicato di un URL. Se il percorso finisce con un'estensione di
/// file nota (es. /foo.jpg?token=…), la query è quasi sempre un token variabile
/// e va IGNORATA, così lo stesso file da pagine diverse è riconosciuto uguale.
/// Se invece il percorso NON ha estensione (es. /img?id=42), la query di solito
/// IDENTIFICA il file e va TENUTA, per non scartare file diversi come duplicati.
pub fn dedup_key(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url).to_lowercase();
    let has_known_ext = VIDEO_EXT
        .iter()
        .chain(IMAGE_EXT.iter())
        .any(|e| path.ends_with(e))
        || path.ends_with(".mp3");
    if has_known_ext {
        path
    } else {
        url.to_lowercase()
    }
}

/// Elenca TUTTI i link contenuti in una pagina/thread (pagine + file diretti),
/// deduplicati, usando gallery-dl in modalità "solo elenco" (-g) con il filtro
/// anti-profilo. Il chiamante li classifica per tipo.
pub async fn list_thread_links(
    app: &AppHandle,
    thread_url: &str,
    cookies_browser: &str,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["-g".into()];
    if cookies_browser == "firefox" {
        args.push("--cookies-from-browser".into());
        args.push("firefox".into());
    }
    args.push("--chapter-filter".into());
    args.push("category not in ('tiktok','instagram','twitter','x','reddit','pinterest','tumblr','youtube','fanbox','patreon')".into());
    args.push(thread_url.into());

    let Ok(cmd) = app.shell().sidecar("gallery-dl") else {
        return Vec::new();
    };
    let output = cmd.args(args).output().await;
    let Ok(out) = output else { return Vec::new() };

    let mut seen = std::collections::HashSet::new();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.starts_with("http"))
        .filter(|l| seen.insert(l.clone())) // dedup
        .collect()
}

/// Client HTTP condiviso per tutti i download di una coda: riusa le connessioni
/// (keep-alive) invece di rifare l'handshake TLS per ogni file. Da creare una
/// volta e passare a download_file.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0")
        .build()
        .unwrap_or_default()
}

/// Oltre questa soglia il file NON viene tenuto in memoria ma scritto a pezzi
/// mentre arriva. Le immagini stanno abbondantemente sotto (e per loro il
/// contenuto in RAM serve alla deduplica); un video diretto da un forum può
/// pesare centinaia di MB e, con 4 download in parallelo più la copia per la
/// dedup, farebbe esplodere la memoria.
const MAX_IN_MEMORY: usize = 32 * 1024 * 1024; // 32 MB

/// Nome file sicuro su Windows, ricavato dall'URL.
/// Sostituisce i caratteri vietati (`\ / : * ? " < > |`, come `make_dir`) e
/// tronca i nomi assurdamente lunghi mantenendo l'estensione: senza, la
/// scrittura fallirebbe in silenzio e il file andrebbe perso.
fn safe_file_name(url: &str) -> String {
    let raw = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    // Percent-decoding minimo dei casi comuni (%20 = spazio)
    let raw = raw.replace("%20", " ");
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 => '_', // caratteri di controllo
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    if cleaned.is_empty() {
        return "file".into();
    }
    // Windows: 255 caratteri per componente del percorso. Tronco il corpo,
    // non l'estensione (che serve a riconoscere il tipo di file).
    const MAX_LEN: usize = 120;
    if cleaned.chars().count() <= MAX_LEN {
        return cleaned;
    }
    let (stem, ext) = match cleaned.rsplit_once('.') {
        Some((s, e)) if e.len() <= 8 => (s, format!(".{e}")),
        _ => (cleaned.as_str(), String::new()),
    };
    let keep = MAX_LEN.saturating_sub(ext.chars().count());
    let stem: String = stem.chars().take(keep).collect();
    format!("{stem}{ext}")
}

/// Percorso libero: se il nome è già occupato da un ALTRO file, aggiunge un
/// contatore — `foto.jpg` → `foto (2).jpg` — come fa Windows. Prima il file
/// veniva sovrascritto in silenzio: due immagini diverse con lo stesso nome
/// (capita spesso nei thread) e la seconda cancellava la prima.
fn unique_path(dir: &Path, name: &str) -> std::path::PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (name, String::new()),
    };
    for n in 2..1000 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// Scarica un file diretto (già risolto) via HTTP in `output_dir`.
/// Se `dedup` è dato, scarta i file duplicati (identici o percettivamente uguali).
/// Restituisce true se il file è stato effettivamente salvato.
pub async fn download_file(
    client: &reqwest::Client,
    url: &str,
    output_dir: &str,
    subdir: &str,
    dedup: Option<&crate::dedup::Dedup>,
) -> bool {
    // Backoff sul 429 (troppe richieste): riprova qualche volta con attesa
    // crescente invece di scartare subito il file.
    let mut resp = None;
    for attempt in 0..3 {
        match client.get(url).send().await {
            Ok(r) if r.status().is_success() => {
                resp = Some(r);
                break;
            }
            Ok(r) if r.status().as_u16() == 429 => {
                // attesa crescente: 1s, 2s, 4s
                tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
            }
            _ => return false,
        }
    }
    let Some(mut resp) = resp else { return false };

    let dir = if subdir.is_empty() {
        Path::new(output_dir).to_path_buf()
    } else {
        Path::new(output_dir).join(subdir)
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let name = safe_file_name(url);

    // Accumulo in memoria SOLO finché resto sotto la soglia: appena la supero
    // (o se il server dichiara subito un file grosso) passo alla scrittura a
    // pezzi su disco. Così un video da centinaia di MB non entra mai tutto in
    // RAM — con 4 download in parallelo sarebbe un disastro. Nota: non mi fido
    // del solo content-length, che può mancare; il controllo è sui byte veri.
    let declared_big = resp
        .content_length()
        .is_some_and(|len| len as usize > MAX_IN_MEMORY);

    let mut buf: Vec<u8> = Vec::new();
    let mut streaming: Option<(std::fs::File, std::path::PathBuf)> = None;
    // Impronta esatta calcolata mentre il file scorre: così anche un file
    // grosso, che non passa mai intero per la memoria, resta deduplicabile.
    let mut hasher = crate::dedup::Fnv::default();
    if declared_big {
        let path = unique_path(&dir, &name);
        let Ok(file) = std::fs::File::create(&path) else {
            return false;
        };
        streaming = Some((file, path));
    }

    use std::io::Write;
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(_) => {
                // Scaricamento interrotto: niente file monchi in giro
                if let Some((_, path)) = &streaming {
                    let _ = std::fs::remove_file(path);
                }
                return false;
            }
        };
        match &mut streaming {
            Some((file, path)) => {
                hasher.update(&chunk);
                if file.write_all(&chunk).is_err() {
                    // Scrittura fallita (tipicamente disco pieno)
                    let _ = std::fs::remove_file(path);
                    return false;
                }
            }
            None => {
                buf.extend_from_slice(&chunk);
                // Superata la soglia: travaso su disco e proseguo in streaming
                if buf.len() > MAX_IN_MEMORY {
                    let path = unique_path(&dir, &name);
                    let Ok(mut file) = std::fs::File::create(&path) else {
                        return false;
                    };
                    if file.write_all(&buf).is_err() {
                        let _ = std::fs::remove_file(&path);
                        return false;
                    }
                    hasher.update(&buf); // anche i byte già accumulati contano
                    buf = Vec::new(); // libero subito la memoria
                    streaming = Some((file, path));
                }
            }
        }
    }

    // Via streaming (file grosso, tipicamente un video): è già su disco.
    // Il controllo dei doppioni avviene ORA, con l'impronta calcolata strada
    // facendo: se l'abbiamo già scaricato in questa coda, il file va via.
    // Solo uguaglianza esatta — il confronto percettivo guarda le immagini,
    // su un video non avrebbe senso.
    if let Some((mut file, path)) = streaming {
        if file.flush().is_err() {
            let _ = std::fs::remove_file(&path);
            return false;
        }
        drop(file); // chiudo prima di poterlo eventualmente cancellare
        if let Some(d) = dedup {
            if !d.keep_exact(hasher.finish()) {
                let _ = std::fs::remove_file(&path);
                return false;
            }
        }
        return true;
    }

    // File piccolo (immagini): il contenuto è in memoria, serve alla deduplica.
    let bytes = buf;

    // Deduplica: se è un doppione (esatto o percettivo), non lo salviamo.
    // keep() decodifica l'immagine e calcola l'hash percettivo: lavoro CPU
    // pesante che bloccherebbe il runtime async e serializzerebbe i download
    // paralleli. Lo eseguiamo su spawn_blocking (thread pool dedicato) così gli
    // altri download continuano mentre questo viene deduplicato.
    if let Some(d) = dedup {
        let d = d.clone();
        let bytes_owned = bytes.clone();
        let keep = tokio::task::spawn_blocking(move || d.keep(&bytes_owned))
            .await
            .unwrap_or(true);
        if !keep {
            return false;
        }
    }

    let path = unique_path(&dir, &name);
    std::fs::write(&path, &bytes).is_ok()
}

/// Cosa cercare nella pagina risolta.
#[derive(Clone, Copy, PartialEq)]
pub enum Want {
    Image,
    Video,
    Any,
}

/// JS iniettato: cerca il media reale della pagina (video o immagine più grande),
/// criterio generico per qualsiasi host senza conoscerne il nome, e ne mette
/// l'URL nell'hash. L'hash non viene toccato dalle pagine (a differenza del titolo).
fn extractor_js(want: Want) -> String {
    // want_video / want_image controllano cosa accettare
    let (want_video, want_image) = match want {
        Want::Image => (false, true),
        Want::Video => (true, false),
        Want::Any => (true, true),
    };
    format!(
        r#"
    (function() {{
      const WANT_VIDEO = {want_video}, WANT_IMAGE = {want_image};
      let tries = 0;
      function findVideo() {{
        for (const v of document.querySelectorAll('video')) {{
          const s = v.currentSrc || v.src || '';
          if (s && !s.startsWith('data:')) return s;
          const src = v.querySelector('source');
          if (src && src.src) return src.src;
        }}
        return null;
      }}
      function findImage() {{
        // La foto vera è l'immagine renderizzata più grande, escluse le anteprime
        let best = null, bestArea = 0;
        for (const i of document.querySelectorAll('img')) {{
          const area = (i.naturalWidth || 0) * (i.naturalHeight || 0);
          const s = i.src || '';
          if (!s || s.includes('logo') || s.startsWith('data:')) continue;
          if (area > bestArea && i.naturalWidth >= 400) {{ best = s; bestArea = area; }}
        }}
        return best;
      }}
      // Tick a 150ms: reagisce prima sulle pagine veloci (la maggioranza). Il
      // limite di tentativi tiene la stessa pazienza totale (~10s) sulle lente.
      const timer = setInterval(() => {{
        tries++;
        let url = null;
        if (WANT_VIDEO) url = findVideo();
        if (!url && WANT_IMAGE) url = findImage();
        if (url) {{ clearInterval(timer); location.hash = 'SCRAPER_RESULT=' + encodeURIComponent(url); }}
        else if (tries > 66) {{ clearInterval(timer); location.hash = 'SCRAPER_RESULT=NULL'; }}
      }}, 150);
    }})();
    "#
    )
}

/// Decodifica percent-encoding (%XX) di base per l'URL passato via location.hash
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Applica alla finestra i flag Win32 che le impediscono di attivarsi o di
/// comparire nell'Alt-Tab: così non ruba il primo piano a un gioco fullscreen.
#[cfg(windows)]
fn make_non_activating(window: &tauri::WebviewWindow) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };
    if let Ok(hwnd) = window.hwnd() {
        let hwnd = hwnd.0 as *mut core::ffi::c_void;
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                ex | (WS_EX_NOACTIVATE as isize) | (WS_EX_TOOLWINDOW as isize),
            );
        }
    }
}

#[cfg(not(windows))]
fn make_non_activating(_window: &tauri::WebviewWindow) {}

/// Contatore globale per label univoci: due resolver non devono mai collidere.
static RESOLVER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Risolve un URL "protetto da JS" nell'URL diretto del file. `None` se fallisce.
/// `want` decide se cercare un video, un'immagine o entrambi.
pub async fn resolve(app: &AppHandle, page_url: &str, _id: usize, want: Want) -> Option<String> {
    let seq = RESOLVER_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let label = format!("resolver-{seq}");

    // Parte invisibile, poi la spostiamo fuori schermo e la mostriamo lì: deve
    // renderizzare (i browser sospendono il JS delle finestre nascoste), ma non
    // deve vedersi. Senza barra del titolo e piccola.
    let window =
        WebviewWindowBuilder::new(app, &label, WebviewUrl::External(page_url.parse().ok()?))
            .visible(false)
            .skip_taskbar(true)
            .focused(false)
            .decorations(false)
            .inner_size(300.0, 300.0)
            .initialization_script(&extractor_js(want))
            .build()
            .ok()?;

    let _ = window.set_position(LogicalPosition::new(-32000.0, -32000.0));
    // Blindatura anti-alt-tab: la finestra non deve MAI attivarsi né apparire
    // nell'Alt-Tab, altrimenti in un gioco fullscreen esclusivo (es. CS2) causerebbe
    // un fastidioso ritorno al desktop. WS_EX_NOACTIVATE + TOOLWINDOW fanno questo.
    make_non_activating(&window);
    let _ = window.show();

    // Polling sull'hash a 150ms: legge il risultato prima appena il JS lo scrive.
    // 90 tentativi × 150ms ≈ 13,5s max, in linea con la pazienza precedente.
    let mut result = None;
    for _ in 0..90 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if let Ok(u) = window.url() {
            if let Some(rest) = u.fragment().and_then(|f| f.strip_prefix("SCRAPER_RESULT=")) {
                let decoded = percent_decode(rest);
                result = (decoded != "NULL").then_some(decoded);
                break;
            }
        }
    }

    // destroy() forza la chiusura e il rilascio del processo WebView2 (close() da
    // solo lascia processi zombie che accumulano memoria). Piccola pausa per dare
    // tempo a WebView2 di liberare davvero prima di aprire la finestra successiva.
    let _ = window.destroy();
    tokio::time::sleep(Duration::from_millis(150)).await;
    result
}


