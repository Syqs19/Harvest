//! Persistenza su disco: cronologia dei download e coda interrotta.
//! Due file JSON nella cartella di configurazione dell'app (accanto a
//! server-pin.txt): history.json (storico dei task conclusi) e queue.json
//! (fotografia della coda in corso, cancellata a coda finita: se c'è
//! all'avvio, la sessione precedente si è interrotta a metà).
//! Scritture "best effort": un errore di scrittura non blocca mai un download.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Tetto della cronologia: oltre, le voci più vecchie vengono scartate.
pub const HISTORY_MAX: usize = 500;

/// Una voce di cronologia: un task concluso (ok/fallito/vuoto).
/// L'URL è quello incollato dall'utente (suo, non un dato da nascondere).
/// Per i task immagini (forum) i campi anteprima restano None: regola
/// privacy, niente titolo né provenienza.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub url: String,
    pub engine: String,
    pub outcome: String,
    /// Motivo neutro del fallimento (solo quando outcome = "failed")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Quando è finito il task (secondi da UNIX_EPOCH)
    pub when: u64,
    /// Cartella di destinazione usata (per "Apri cartella")
    pub dir: String,
    /// Percorso del file prodotto, se il task ne ha prodotto UNO solo
    /// (per "Mostra file"; per playlist/gallerie resta None)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    // Anteprima (solo video): titolo, autore, durata, miniatura
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploader: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
}

/// Coda salvata su disco: i task NON ancora completati e i parametri per
/// riprenderli identici (stesso tipo, formato, velocità, login, cartella).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedQueue {
    pub tasks: Vec<SavedTask>,
    pub video_mode: String,
    /// Contenitore/codec video: "auto" | "mp4" | "editing"
    #[serde(default = "default_video_format")]
    pub video_format: String,
    /// Tetto di risoluzione (0 = massima disponibile)
    #[serde(default)]
    pub max_height: u16,
    #[serde(default)]
    pub audio_format: String,
    /// Arricchimento: tag, copertina, capitoli (default true per code vecchie)
    #[serde(default = "default_enrich")]
    pub enrich: bool,
    /// Sottotitoli: "no" | "embed" | "file" | "both"
    #[serde(default = "default_subs")]
    pub subs: String,
    pub concurrency: u8,
    #[serde(default)]
    pub cookies_browser: String,
    pub output_dir: String,
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedTask {
    pub url: String,
    pub engine: String,
}

/// Secondi da UNIX_EPOCH (per il campo `when` della cronologia).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn history_file(dir: &Path) -> PathBuf {
    dir.join("history.json")
}

fn queue_file(dir: &Path) -> PathBuf {
    dir.join("queue.json")
}

/// Scrittura atomica: prima su file temporaneo, poi rename. Così un crash
/// a metà scrittura non lascia mai un JSON troncato al posto di quello buono.
fn write_json<T: serde::Serialize>(path: &Path, value: &T) {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Cronologia dal disco; file assente o illeggibile = cronologia vuota.
pub fn load_history(dir: &Path) -> Vec<HistoryEntry> {
    std::fs::read_to_string(history_file(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_history(dir: &Path, entries: &[HistoryEntry]) {
    write_json(&history_file(dir), &entries);
}

/// Coda interrotta dal disco; None se assente, illeggibile o senza task.
pub fn load_queue(dir: &Path) -> Option<SavedQueue> {
    let q: SavedQueue = std::fs::read_to_string(queue_file(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())?;
    (!q.tasks.is_empty()).then_some(q)
}

pub fn save_queue(dir: &Path, queue: &SavedQueue) {
    write_json(&queue_file(dir), queue);
}

pub fn delete_queue(dir: &Path) {
    let _ = std::fs::remove_file(queue_file(dir));
}
