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

/// Scarica un file diretto (già risolto) via HTTP in `output_dir`.
/// Se `dedup` è dato, scarta i file duplicati (identici o percettivamente uguali).
/// Restituisce true se il file è stato effettivamente salvato.
pub async fn download_file(
    url: &str,
    output_dir: &str,
    subdir: &str,
    dedup: Option<&crate::dedup::Dedup>,
) -> bool {
    let client = match reqwest::Client::builder().user_agent("Mozilla/5.0").build() {
        Ok(c) => c,
        Err(_) => return false,
    };
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
    let Some(resp) = resp else { return false };

    // Scarica in memoria (i file media sono piccoli): serve il contenuto completo
    // per la deduplica prima di scriverlo su disco.
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(_) => return false,
    };

    // Deduplica: se è un doppione (esatto o percettivo), non lo salviamo.
    if let Some(d) = dedup {
        if !d.keep(&bytes) {
            return false;
        }
    }

    // Nome file dall'URL. subdir vuoto = direttamente nella cartella scelta.
    let name = url
        .rsplit('/')
        .next()
        .unwrap_or("file")
        .split('?')
        .next()
        .unwrap_or("file");
    let dir = if subdir.is_empty() {
        Path::new(output_dir).to_path_buf()
    } else {
        Path::new(output_dir).join(subdir)
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = dir.join(name);

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
      const timer = setInterval(() => {{
        tries++;
        let url = null;
        if (WANT_VIDEO) url = findVideo();
        if (!url && WANT_IMAGE) url = findImage();
        if (url) {{ clearInterval(timer); location.hash = 'SCRAPER_RESULT=' + encodeURIComponent(url); }}
        else if (tries > 50) {{ clearInterval(timer); location.hash = 'SCRAPER_RESULT=NULL'; }}
      }}, 200);
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

    // Polling sull'hash: max ~14s (le pagine JS possono metterci qualche secondo)
    let mut result = None;
    for _ in 0..70 {
        tokio::time::sleep(Duration::from_millis(200)).await;
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
