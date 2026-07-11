//! Aggiornamento dei motori (i sidecar che fanno il lavoro vero).
//!
//! Solo yt-dlp è aggiornabile, ed è una scelta obbligata:
//! - yt-dlp pubblica `yt-dlp.exe` in ogni release e sa auto-aggiornarsi con `-U`.
//!   È anche l'unico che invecchia in fretta: quando i siti cambiano, un yt-dlp
//!   vecchio smette di funzionare nel giro di settimane.
//! - gallery-dl dalla 1.32 NON pubblica più l'eseguibile Windows nelle sue
//!   release: il suo `-U` scarica un 404 e — insidioso — esce comunque con
//!   codice 0, cioè segnala "riuscito" senza aver aggiornato nulla. Escluso.
//! - ffmpeg non ha auto-update, cambia raramente e pesa ~100 MB. Escluso.
//!
//! DOVE finisce l'aggiornamento. Il motore si aggiorna sovrascrivendo il proprio
//! .exe, quindi serve poter scrivere nella cartella dove sta. Quando l'app è
//! installata in Programmi quella cartella è di sola lettura (servirebbero i
//! permessi di amministratore, cioè un popup UAC a ogni aggiornamento: da
//! evitare). Allora: se la cartella dei sidecar è scrivibile aggiorniamo lì
//! (nessuna copia); altrimenti copiamo il motore in `engines/` dentro la
//! cartella dati dell'app e aggiorniamo la copia. Da quel momento l'app usa la
//! copia. Il sidecar originale resta intatto: se un aggiornamento esce guasto,
//! basta svuotare `engines/` per tornare alla versione dell'installer.

use std::path::{Path, PathBuf};
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

/// Release più recente di yt-dlp (solo il tag, ~1 KB di JSON).
const YTDLP_LATEST_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";

/// Nome dell'eseguibile aggiornato dentro `engines/`.
fn ytdlp_exe_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}

/// Cartella dei motori aggiornati, dentro la cartella dati dell'app.
fn engines_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("engines")
}

/// Percorso del motore aggiornato, se è già stato scaricato una volta.
pub fn updated_ytdlp(config_dir: &Path) -> Option<PathBuf> {
    let p = engines_dir(config_dir).join(ytdlp_exe_name());
    p.is_file().then_some(p)
}

/// Il motore da lanciare: la copia aggiornata in `engines/` se c'è, altrimenti
/// il sidecar incluso nell'installer. UNICO punto in cui si decide quale
/// eseguibile gira: chiamalo al posto di `app.shell().sidecar("yt-dlp")`.
pub fn ytdlp_command(
    app: &AppHandle,
    config_dir: &Path,
) -> Result<tauri_plugin_shell::process::Command, String> {
    match updated_ytdlp(config_dir) {
        Some(path) => Ok(app.shell().command(path.to_string_lossy().to_string())),
        None => app.shell().sidecar("yt-dlp").map_err(|e| e.to_string()),
    }
}

/// Stato di un motore per la UI: versione in uso e, se c'è, quella disponibile.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    /// Nome tecnico (yt-dlp): la UI mostra "Motore video", questo in secondo piano
    pub name: String,
    /// Versione attualmente in uso, None se il motore non risponde
    pub current: Option<String>,
    /// Ultima versione pubblicata, None se il controllo non è riuscito (offline)
    pub latest: Option<String>,
    /// C'è davvero qualcosa da aggiornare
    pub update_available: bool,
}

/// Versione del motore in uso (quello che verrebbe davvero lanciato).
async fn current_version(app: &AppHandle, config_dir: &Path) -> Option<String> {
    let out = ytdlp_command(app, config_dir)
        .ok()?
        .args(["--version"])
        .output()
        .await
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// Ultima versione pubblicata. Solo il tag della release: una richiesta piccola,
/// senza scaricare l'eseguibile. Se la rete non c'è, None (nessuna pillola).
async fn latest_version() -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }
    let client = crate::resolver::http_client();
    let body = client
        .get(YTDLP_LATEST_API)
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let r: Release = serde_json::from_str(&body).ok()?;
    let v = r.tag_name.trim_start_matches('v').to_string();
    (!v.is_empty()).then_some(v)
}

/// Controlla se il motore video ha una versione più recente. Non scarica nulla.
/// Usato all'avvio (silenzioso: se fallisce, semplicemente non compare nulla)
/// e dal bottone di controllo manuale.
pub async fn check(app: &AppHandle, config_dir: &Path) -> EngineInfo {
    let current = current_version(app, config_dir).await;
    let latest = latest_version().await;
    // Aggiornamento disponibile solo se conosco entrambe e differiscono.
    // Confronto per uguaglianza, non "maggiore di": i tag di yt-dlp sono date
    // (2026.07.04) e un confronto testuale su versioni diverse basta; così
    // non prometto aggiornamenti quando sono offline o il motore non risponde.
    let update_available = match (&current, &latest) {
        (Some(c), Some(l)) => c != l,
        _ => false,
    };
    EngineInfo {
        name: "yt-dlp".into(),
        current,
        latest,
        update_available,
    }
}

/// La cartella è scrivibile senza permessi di amministratore?
/// Provo a creare un file lì dentro: l'unico modo affidabile su Windows
/// (i flag di sola lettura e le ACL non si leggono in modo portabile).
fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".harvest-write-test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Aggiorna il motore video all'ultima versione.
///
/// Se la cartella dei sidecar è scrivibile, aggiorna direttamente lì (nessuna
/// copia, nessuno spreco di disco). Se non lo è (app installata in Programmi),
/// copia il motore in `engines/` e aggiorna la copia.
///
/// Restituisce la nuova versione. Errore se l'aggiornamento non è andato a buon
/// fine: NON mi fido del codice di uscita, che yt-dlp tiene a 0 anche quando il
/// download fallisce — verifico che la versione sia davvero cambiata.
pub async fn update(app: &AppHandle, config_dir: &Path) -> Result<String, String> {
    let before = current_version(app, config_dir).await;

    // Il motore da aggiornare: la copia se esiste già, altrimenti il sidecar.
    // Se il sidecar sta in una cartella di sola lettura, prima me lo copio.
    let target: PathBuf = match updated_ytdlp(config_dir) {
        Some(p) => p,
        None => {
            let sidecar = sidecar_path(app)?;
            let dir = sidecar.parent().ok_or("Engine not found")?;
            if is_writable(dir) {
                // Cartella scrivibile: aggiorno il motore dov'è, senza duplicarlo
                sidecar
            } else {
                // Sola lettura (tipico dell'app installata): copio e aggiorno la copia
                let dest_dir = engines_dir(config_dir);
                std::fs::create_dir_all(&dest_dir)
                    .map_err(|e| format!("Couldn't create the engines folder: {e}"))?;
                let dest = dest_dir.join(ytdlp_exe_name());
                std::fs::copy(&sidecar, &dest)
                    .map_err(|e| format!("Couldn't copy the engine: {e}"))?;
                dest
            }
        }
    };

    let out = app
        .shell()
        .command(target.to_string_lossy().to_string())
        .args(["-U"])
        .output()
        .await
        .map_err(|e| format!("Update failed: {e}"))?;

    let after = current_version(app, config_dir).await;
    match after {
        // La versione è cambiata: aggiornamento riuscito davvero.
        Some(v) if Some(&v) != before.as_ref() => Ok(v),
        // Nessun cambiamento. Può essere "era già aggiornato" (raro: la pillola
        // compare solo se c'era differenza) o un download fallito in silenzio.
        _ => {
            let log = String::from_utf8_lossy(&out.stdout);
            let err = String::from_utf8_lossy(&out.stderr);
            if log.contains("up to date") {
                Err("The engine is already up to date".into())
            } else {
                // Messaggio neutro: niente URL né log grezzi davanti all'utente.
                let _ = err;
                Err("Update failed: try again later".into())
            }
        }
    }
}

/// Motori che devono esserci perché l'app funzioni: nome del file e nome
/// "parlante" da mostrare all'utente se manca.
const REQUIRED: &[(&str, &str)] = &[
    ("yt-dlp", "video engine"),
    ("gallery-dl", "image engine"),
    ("ffmpeg", "audio/video merging"),
];

/// Motori mancanti accanto all'eseguibile. Restituisce i nomi "parlanti" di
/// quelli che non ci sono. Vuoto = tutto a posto.
///
/// Serve all'avvio: se un antivirus mette in quarantena un motore (capita: gli
/// eseguibili PyInstaller vengono spesso segnalati a torto), senza questo
/// controllo l'app sembra funzionare e fallisce solo al primo download, con un
/// errore generico che non spiega niente.
pub fn missing_engines(config_dir: &Path) -> Vec<String> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new(); // non so dove sono: meglio non allarmare
    };
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    REQUIRED
        .iter()
        .filter(|(file, _)| {
            let name = if cfg!(windows) {
                format!("{file}.exe")
            } else {
                file.to_string()
            };
            // yt-dlp vale anche se c'è solo la copia aggiornata in engines/
            let updated = *file == "yt-dlp" && updated_ytdlp(config_dir).is_some();
            !dir.join(&name).is_file() && !updated
        })
        .map(|(_, label)| (*label).to_string())
        .collect()
}

/// Percorso del sidecar incluso nell'installer (accanto all'eseguibile dell'app).
/// Tauri copia i sidecar lì rinominandoli senza il suffisso della piattaforma.
fn sidecar_path(app: &AppHandle) -> Result<PathBuf, String> {
    let _ = app;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("App folder not found")?;
    let p = dir.join(ytdlp_exe_name());
    p.is_file()
        .then_some(p)
        .ok_or_else(|| "Video engine not found".into())
}
