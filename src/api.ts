// Strato API unico: ogni chiamata al backend passa da qui.
// Dentro l'app desktop usa l'IPC di Tauri; aperta dal browser del telefono
// (modalità server) usa HTTP/WebSocket verso la porta del server integrato.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

export type Engine = "video" | "images";
export type VideoMode = "full" | "videoOnly" | "audioOnly";
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

export type DownloadEvent =
  | { kind: "queueStart"; tasks: { url: string; engine: Engine }[] }
  | {
      kind: "itemStart";
      index: number;
      total: number;
      url: string;
      engine: Engine;
    }
  | { kind: "progress"; index: number; percent: number }
  | { kind: "line"; index: number; line: string }
  | { kind: "itemDone"; index: number; outcome: Outcome }
  | {
      kind: "finished";
      ok: number;
      failed: number;
      nothing: number;
      cancelled: boolean;
    };

export interface ServerSnapshot {
  busy: boolean;
  timeline: { url: string; engine: Engine; status: string }[];
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
  cookiesBrowser: CookiesBrowser,
  outputDir: string,
): Promise<string> {
  if (isRemote) {
    await http("/api/start", {
      method: "POST",
      body: JSON.stringify({
        links,
        video,
        images,
        videoMode,
        cookiesBrowser,
        outputDir,
      }),
    });
    return "Avviato";
  }
  return invoke<string>("start_download", {
    links,
    video,
    images,
    videoMode,
    cookiesBrowser,
    outputDir,
  });
}

export async function cancelDownload(): Promise<void> {
  if (isRemote) {
    await http("/api/cancel", { method: "POST" });
    return;
  }
  return invoke("cancel_download");
}

export function onDownloadEvent(
  cb: (ev: DownloadEvent) => void,
): Promise<UnlistenFn> {
  if (isRemote) {
    const ws = new WebSocket(
      `ws://${location.host}/api/events?pin=${encodeURIComponent(getPin())}`,
    );
    ws.onmessage = (e) => cb(JSON.parse(e.data));
    return Promise.resolve(() => ws.close());
  }
  return listen<DownloadEvent>("download-event", (e) => cb(e.payload));
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
 * Controlla se c'è un aggiornamento e, se sì, lo scarica e installa.
 * `onState` riceve gli stati per aggiornare la UI. Solo app desktop.
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
