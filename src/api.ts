// Strato API unico: ogni chiamata al backend passa da qui.
// Dentro l'app desktop usa l'IPC di Tauri; aperta dal browser del telefono
// (modalità server) usa HTTP/WebSocket verso la porta del server integrato.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

export type Engine = "video" | "images";
export type VideoMode = "full" | "videoOnly" | "audioOnly";
export type AudioFormat = "mp3" | "wav" | "opus";
/** Contenitore/codec video: originale, MP4 (remux) o MP4+H.264 per l'editing */
export type VideoFormat = "auto" | "mp4" | "editing";
/** Sottotitoli: nessuno, nel video, file .srt separato, entrambi */
export type SubsMode = "no" | "embed" | "file" | "both";
export type Outcome = "ok" | "failed" | "nothing";
export type CookiesBrowser = "" | "firefox";

const isTauri = "__TAURI_INTERNALS__" in window;
/** true quando la UI gira nel browser del telefono invece che nell'app */
export const isRemote = !isTauri;

const PIN_KEY = "serverPin";
export const getPin = () => localStorage.getItem(PIN_KEY) ?? "";
export const savePin = (pin: string) => localStorage.setItem(PIN_KEY, pin);

/** PIN mancante o errato: la UI deve chiederlo all'utente */
export class PinError extends Error {}

// Il telefono riceve la UI dal server stesso, quindi le chiamate API
// vanno alla stessa origine (host e porta) da cui è arrivata la pagina.
async function http(path: string, init?: RequestInit): Promise<Response> {
  const res = await fetch(path, {
    ...init,
    headers: { "content-type": "application/json", "x-pin": getPin() },
  });
  if (res.status === 401) throw new PinError();
  if (!res.ok) throw new Error(await res.text());
  return res;
}

/** Metadati d'anteprima (solo video): titolo, autore, durata, miniatura. */
export interface Preview {
  title?: string;
  uploader?: string;
  duration?: number; // secondi
  thumbnail?: string;
}

export type DownloadEvent =
  | { kind: "queueStart"; tasks: { url: string; engine: Engine }[] }
  | {
      kind: "itemStart";
      index: number;
      total: number;
      url: string;
      engine: Engine;
    }
  | { kind: "queueAppend"; baseIndex: number; tasks: { url: string; engine: Engine }[] }
  | ({ kind: "preview"; index: number } & Preview)
  | { kind: "phase"; index: number; phase: string }
  | {
      kind: "progress";
      index: number;
      percent: number;
      speed?: number; // byte/s
      eta?: number; // secondi
      downloaded?: number; // byte
      total?: number; // byte
    }
  | { kind: "line"; index: number; line: string }
  | {
      kind: "itemDone";
      index: number;
      outcome: Outcome;
      reason?: string;
      /** cartella di destinazione del task (per "Apri cartella") */
      dir: string;
      /** percorso del file prodotto, se unico (per "Mostra file") */
      filePath?: string;
    }
  | {
      kind: "finished";
      ok: number;
      failed: number;
      nothing: number;
      cancelled: boolean;
    };

export interface ServerSnapshot {
  busy: boolean;
  timeline: ({
    url: string;
    engine: Engine;
    status: string;
    reason?: string;
  } & Preview)[];
  lastOutputDir: string;
}

export interface DirEntry {
  name: string;
  path: string;
}

export interface DirListing {
  path: string | null;
  parent: string | null;
  entries: DirEntry[];
  shortcuts: DirEntry[];
}

/** Naviga le cartelle del PC (per il selettore usato dal telefono) */
export async function browseDir(path: string | null): Promise<DirListing> {
  if (isRemote) {
    const q = path ? `?path=${encodeURIComponent(path)}` : "";
    const res = await http("/api/browse" + q);
    return res.json();
  }
  return invoke<DirListing>("browse_dir", { path });
}

/** Crea una sottocartella e restituisce il contenuto aggiornato */
export async function createDir(
  parent: string,
  name: string,
): Promise<DirListing> {
  if (isRemote) {
    const res = await http("/api/mkdir", {
      method: "POST",
      body: JSON.stringify({ parent, name }),
    });
    return res.json();
  }
  return invoke<DirListing>("create_dir", { parent, name });
}

export interface ServerInfo {
  /** null se il server non è riuscito ad aprire nessuna porta */
  port: number | null;
  pin: string;
  addresses: string[];
}

export async function startDownload(
  links: string[],
  video: boolean,
  images: boolean,
  videoMode: VideoMode,
  videoFormat: VideoFormat,
  maxHeight: number,
  audioFormat: AudioFormat,
  enrich: boolean,
  subs: SubsMode,
  concurrency: number,
  cookiesBrowser: CookiesBrowser,
  outputDir: string,
  append = false,
): Promise<string> {
  if (isRemote) {
    await http("/api/start", {
      method: "POST",
      body: JSON.stringify({
        links,
        video,
        images,
        videoMode,
        videoFormat,
        maxHeight,
        audioFormat,
        enrich,
        subs,
        concurrency,
        cookiesBrowser,
        outputDir,
        append,
      }),
    });
    return "Started";
  }
  return invoke<string>("start_download", {
    links,
    video,
    images,
    videoMode,
    videoFormat,
    maxHeight,
    audioFormat,
    enrich,
    subs,
    concurrency,
    cookiesBrowser,
    outputDir,
    append,
  });
}

/** Una voce della cronologia persistente (un task concluso). */
export interface HistoryEntry {
  url: string;
  engine: Engine;
  outcome: Outcome;
  reason?: string;
  /** quando è finito il task (secondi da epoch) */
  when: number;
  /** cartella di destinazione usata */
  dir: string;
  /** percorso del file prodotto, se unico */
  filePath?: string;
  // Anteprima (solo video)
  title?: string;
  uploader?: string;
  duration?: number;
  thumbnail?: string;
}

/** Cronologia completa, dalla più vecchia alla più recente (solo desktop) */
export async function getHistory(): Promise<HistoryEntry[]> {
  return invoke<HistoryEntry[]>("get_history");
}

export async function clearHistory(): Promise<void> {
  return invoke("clear_history");
}

/** Coda rimasta a metà nella sessione precedente (banner Riprendi/Scarta) */
export interface InterruptedQueue {
  tasks: { url: string; engine: Engine }[];
  outputDir: string;
}

export async function getInterrupted(): Promise<InterruptedQueue | null> {
  if (isRemote) {
    const res = await http("/api/interrupted");
    return res.json();
  }
  return invoke<InterruptedQueue | null>("interrupted_queue");
}

/** Riprende la coda interrotta con i parametri della sessione precedente */
export async function resumeQueue(): Promise<void> {
  if (isRemote) {
    await http("/api/resume", { method: "POST" });
    return;
  }
  return invoke("resume_queue");
}

export async function discardQueue(): Promise<void> {
  if (isRemote) {
    await http("/api/discard", { method: "POST" });
    return;
  }
  return invoke("discard_queue");
}

/** Apre la cartella in Esplora risorse (solo app desktop) */
export async function openFolder(path: string): Promise<void> {
  return invoke("open_folder", { path });
}

/** Apre Esplora risorse con il file già selezionato (solo app desktop) */
export async function revealFile(path: string): Promise<void> {
  return invoke("reveal_file", { path });
}

/** Rimuove un elemento dalla coda/timeline (solo a coda ferma). */
export async function removeItem(index: number): Promise<void> {
  if (isRemote) {
    await http("/api/remove", {
      method: "POST",
      body: JSON.stringify({ index }),
    });
    return;
  }
  return invoke("remove_item", { index });
}

export async function cancelDownload(): Promise<void> {
  if (isRemote) {
    await http("/api/cancel", { method: "POST" });
    return;
  }
  return invoke("cancel_download");
}

/**
 * Ascolta gli eventi del download. Nell'app desktop passa dall'IPC di Tauri.
 *
 * Dal telefono usa un WebSocket, che però cade facilmente (schermo bloccato,
 * Wi-Fi che diventa rete mobile, standby): perciò si RICONNETTE da solo, con
 * attesa crescente per non martellare il PC. `onLink` segnala lo stato del
 * collegamento: alla riconnessione chi ascolta deve ricaricare lo stato dal
 * server, perché gli eventi persi nel frattempo non tornano più indietro.
 */
export function onDownloadEvent(
  cb: (ev: DownloadEvent) => void,
  onLink?: (connected: boolean) => void,
): Promise<UnlistenFn> {
  if (!isRemote) {
    return listen<DownloadEvent>("download-event", (e) => cb(e.payload));
  }

  let ws: WebSocket | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let attempt = 0;
  let closed = false; // la pagina ha chiuso l'ascolto: non riconnettere più

  const connect = () => {
    if (closed) return;
    ws = new WebSocket(
      `ws://${location.host}/api/events?pin=${encodeURIComponent(getPin())}`,
    );
    ws.onopen = () => {
      attempt = 0;
      onLink?.(true);
    };
    ws.onmessage = (e) => cb(JSON.parse(e.data));
    // onclose scatta anche dopo un errore: un solo punto per ritentare
    ws.onclose = () => {
      if (closed) return;
      onLink?.(false);
      // Attesa crescente 1s, 2s, 4s… fino a 15s: rapido sui cali brevi,
      // discreto se il PC è spento davvero.
      const wait = Math.min(1000 * 2 ** attempt++, 15000);
      timer = setTimeout(connect, wait);
    };
  };

  // Il telefono si è appena risvegliato o è tornata la rete: ritenta SUBITO
  // invece di aspettare il prossimo tentativo (che può essere a 15 secondi).
  const wakeUp = () => {
    if (closed || document.visibilityState !== "visible") return;
    if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING))
      return;
    clearTimeout(timer);
    attempt = 0;
    connect();
  };
  document.addEventListener("visibilitychange", wakeUp);
  window.addEventListener("online", wakeUp);

  connect();

  return Promise.resolve(() => {
    closed = true;
    clearTimeout(timer);
    document.removeEventListener("visibilitychange", wakeUp);
    window.removeEventListener("online", wakeUp);
    ws?.close();
  });
}

/** Fotografia della coda per chi si collega a download già avviati (solo remoto) */
export async function fetchServerState(): Promise<ServerSnapshot> {
  const res = await http("/api/state");
  return res.json();
}

/** Dati del pannello "Accesso dal telefono" (solo app desktop) */
export async function fetchServerInfo(): Promise<ServerInfo> {
  return invoke<ServerInfo>("server_info");
}

export type UpdateState =
  | { status: "idle" }
  | { status: "checking" }
  | { status: "none"; version: string }
  | { status: "available"; version: string }
  | { status: "downloading"; percent: number }
  | { status: "ready" }
  | { status: "error"; message: string };

/**
 * Controlla SOLO se c'è un aggiornamento, senza scaricarlo. Non invasivo:
 * si può chiamare all'avvio. Solo app desktop.
 */
export async function checkUpdate(
  onState: (s: UpdateState) => void,
): Promise<void> {
  const { check } = await import("@tauri-apps/plugin-updater");
  onState({ status: "checking" });
  try {
    const update = await check();
    onState(
      update
        ? { status: "available", version: update.version }
        : { status: "none", version: "" },
    );
  } catch (e) {
    onState({ status: "error", message: String(e) });
  }
}

/**
 * Scarica e installa l'aggiornamento, poi riavvia. Da chiamare solo su azione
 * esplicita dell'utente. `onState` riceve gli stati per aggiornare la UI.
 */
export async function runUpdate(
  onState: (s: UpdateState) => void,
): Promise<void> {
  const { check } = await import("@tauri-apps/plugin-updater");
  const { relaunch } = await import("@tauri-apps/plugin-process");
  onState({ status: "checking" });
  try {
    const update = await check();
    if (!update) {
      onState({ status: "none", version: "" });
      return;
    }
    onState({ status: "available", version: update.version });
    let total = 0;
    let got = 0;
    await update.downloadAndInstall((ev) => {
      if (ev.event === "Started") total = ev.data.contentLength ?? 0;
      else if (ev.event === "Progress") {
        got += ev.data.chunkLength;
        onState({
          status: "downloading",
          percent: total ? (got / total) * 100 : 0,
        });
      } else if (ev.event === "Finished") onState({ status: "ready" });
    });
    // Riavvia sull'app aggiornata
    await relaunch();
  } catch (e) {
    onState({ status: "error", message: String(e) });
  }
}

/**
 * Stato del motore video (yt-dlp): versione in uso e, se c'è, quella nuova.
 * È il motore che invecchia in fretta — quando un sito cambia, un motore
 * vecchio smette di funzionare. gallery-dl e ffmpeg non sono aggiornabili
 * (nessun eseguibile pubblicato / nessun auto-update): vedi engines.rs.
 */
export interface EngineInfo {
  name: string;
  current?: string;
  latest?: string;
  updateAvailable: boolean;
}

/**
 * Motori mancanti accanto all'app (tipicamente messi in quarantena da un
 * antivirus). Vuoto = tutto a posto. Controllato all'avvio: senza, l'app
 * sembrerebbe funzionante e fallirebbe solo al primo download.
 */
export async function missingEngines(): Promise<string[]> {
  if (isRemote) return [];
  return invoke<string[]>("missing_engines");
}

/** Controlla se il motore video ha una versione più recente. Non scarica nulla. */
export async function checkEngine(): Promise<EngineInfo> {
  if (isRemote) return (await http("/api/engine")).json();
  return invoke<EngineInfo>("check_engine");
}

/** Aggiorna il motore video. Restituisce la nuova versione. */
export async function updateEngine(): Promise<string> {
  if (isRemote) {
    const res = await http("/api/engine/update", { method: "POST" });
    return (await res.json()).version;
  }
  return invoke<string>("update_engine");
}

/** Avvio automatico con Windows (solo app desktop) */
export async function getAutostart(): Promise<boolean> {
  return invoke<boolean>("autostart_enabled");
}

export async function setAutostart(enabled: boolean): Promise<void> {
  return invoke("set_autostart", { enabled });
}

export async function pickOutputFolder(): Promise<string | null> {
  const selected = await open({ directory: true, multiple: false });
  return typeof selected === "string" ? selected : null;
}

/**
 * Conferma con finestra di dialogo NATIVA (plugin dialog): window.confirm
 * non è affidabile dentro la WebView. Solo app desktop.
 */
export async function askConfirm(message: string): Promise<boolean> {
  const { confirm } = await import("@tauri-apps/plugin-dialog");
  return confirm(message, { title: "Harvest", kind: "warning" });
}

/**
 * Notifica di sistema (toast di Windows). Best-effort: se il permesso manca
 * lo chiede una volta; se negato, silenziosamente non notifica. Solo desktop.
 * NB: comandi IPC diretti del plugin, NON il pacchetto JS: quello passa da
 * window.Notification, che nella nostra WebView resta "denied" fisso
 * (verificato dal vivo); la via IPC invece funziona ed è coperta dalla
 * permission notification:default.
 */
export async function notify(title: string, body: string): Promise<void> {
  if (isRemote) return;
  try {
    let granted = await invoke<boolean>(
      "plugin:notification|is_permission_granted",
    );
    if (!granted)
      granted =
        (await invoke<string>("plugin:notification|request_permission")) ===
        "granted";
    if (granted)
      await invoke("plugin:notification|notify", {
        options: { title, body },
      });
  } catch {
    /* la notifica non deve mai rompere il flusso */
  }
}

/** Versione dell'app (per la schermata Impostazioni). Solo desktop. */
export async function getAppVersion(): Promise<string> {
  const { getVersion } = await import("@tauri-apps/api/app");
  return getVersion();
}
