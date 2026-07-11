import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import QRCode from "react-qr-code";
import {
  startDownload,
  cancelDownload,
  removeItem,
  getHistory,
  clearHistory,
  getInterrupted,
  resumeQueue,
  discardQueue,
  openFolder,
  revealFile,
  onDownloadEvent,
  pickOutputFolder,
  askConfirm,
  notify,
  getAppVersion,
  fetchServerState,
  fetchServerInfo,
  getAutostart,
  setAutostart,
  checkUpdate,
  runUpdate,
  checkEngine,
  updateEngine,
  missingEngines,
  type EngineInfo,
  type UpdateState,
  browseDir,
  createDir,
  isRemote,
  getPin,
  savePin,
  PinError,
  type Engine,
  type VideoMode,
  type VideoFormat,
  type SubsMode,
  type AudioFormat,
  type Outcome,
  type ServerInfo,
  type DirListing,
  type CookiesBrowser,
  type HistoryEntry,
  type InterruptedQueue,
} from "./api";
import {
  findNumbers,
  generateSeriesUrls,
  seriesCount,
  MAX_SERIES,
} from "./urls";

interface TimelineItem {
  url: string;
  engine: Engine;
  status: "pending" | "running" | Outcome;
  reason?: string; // motivo neutro del fallimento (solo status "failed")
  dir?: string; // cartella di destinazione (per "Apri cartella")
  filePath?: string; // percorso del file prodotto, se unico ("Mostra file")
  // Anteprima (solo video), popolata dall'evento "preview"
  title?: string;
  uploader?: string;
  duration?: number; // secondi
  thumbnail?: string;
  // Stato live durante l'elaborazione di questo elemento
  phase?: string; // testo della fase d'analisi ("Leggo i formati…")
  percent?: number | null; // null/undefined = indeterminato
  speed?: number; // byte/s
  eta?: number; // secondi
  downloaded?: number; // byte
  total?: number; // byte
}

function updateLabel(u: UpdateState): string {
  switch (u.status) {
    case "idle":
      return "Check for a new version";
    case "checking":
      return "Checking...";
    case "none":
      return "You're on the latest version";
    case "available":
      return `New version ${u.version} found, downloading...`;
    case "downloading":
      return `Downloading update... ${u.percent.toFixed(0)}%`;
    case "ready":
      return "Update ready: restarting...";
    case "error":
      return `Error: ${u.message}`;
  }
}

/**
 * Comportamento da finestra modale su un contenitore: chiusura con Esc, focus
 * che entra dentro all'apertura, Tab che ci resta intrappolato (senza, si
 * finisce a navigare i bottoni della pagina sotto, che è disorientante) e
 * focus restituito a chi l'ha aperta alla chiusura.
 */
function useModal(onClose: () => void) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    // Chi aveva il focus prima: ce lo riportiamo alla chiusura
    const opener = document.activeElement as HTMLElement | null;
    const focusables = () =>
      [
        ...(ref.current?.querySelectorAll<HTMLElement>(
          'a[href], button:not([disabled]), input:not([disabled]), select, textarea, [tabindex]:not([tabindex="-1"])',
        ) ?? []),
      ].filter((el) => el.offsetParent !== null);

    // Il primo elemento utile riceve il focus (se non l'ha già preso un autoFocus)
    if (!ref.current?.contains(document.activeElement)) focusables()[0]?.focus();

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key !== "Tab") return;
      const items = focusables();
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      // Ciclo: da ultimo→primo in avanti, da primo→ultimo indietro
      if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      } else if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      opener?.focus?.();
    };
  }, [onClose]);
  return ref;
}

/**
 * Involucro della finestra modale: sfondo scurito (clic fuori = chiudi) e
 * riquadro con il comportamento accessibile (Esc, focus trap, focus restituito).
 * Il contenuto lo passa chi la apre.
 */
function Modal({
  onClose,
  label,
  children,
}: {
  onClose: () => void;
  label: string;
  children: ReactNode;
}) {
  const ref = useModal(onClose);
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      onClick={onClose}
    >
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-label={label}
        className="flex max-h-[80vh] w-full max-w-md flex-col rounded-lg border border-line bg-panel"
        onClick={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}

// Pannellino aperto dalla pillola dell'header: una riga per ciò che si può
// aggiornare — l'app e il motore video. Il motore ha un nome funzionale
// ("Motore video"): il nome tecnico resta in secondo piano, accanto alla
// versione, per chi va a cercarne il changelog.
// Gli altri motori non compaiono perché non sono aggiornabili (vedi engines.rs).
function UpdatesPanel({
  onClose,
  update,
  appVersion,
  onAppUpdate,
  engine,
  engineBusy,
  engineMsg,
  onEngineUpdate,
}: {
  onClose: () => void;
  update: UpdateState;
  appVersion: string;
  onAppUpdate: () => void;
  engine: EngineInfo | null;
  engineBusy: boolean;
  engineMsg: string;
  onEngineUpdate: () => void;
}) {
  // Esc, focus dentro e Tab intrappolato (come la modale); in più il clic
  // fuori, che da un popover ci si aspetta.
  const ref = useModal(onClose);
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [onClose, ref]);

  const appWorking =
    update.status === "checking" ||
    update.status === "downloading" ||
    update.status === "ready";
  const appActionable =
    update.status === "available" || update.status === "error";

  return (
    <div
      ref={ref}
      role="dialog"
      aria-label="Updates"
      className="absolute right-0 top-full z-20 mt-2 w-80 rounded-lg border border-line bg-panel p-3 shadow-xl"
    >
      <div className="flex flex-col gap-3">
        {/* App: c'è solo se l'updater ha qualcosa da dire */}
        {update.status !== "idle" && update.status !== "none" && (
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="text-sm font-medium">Harvest</div>
              <div className="truncate font-mono text-xs text-ink-dim">
                {update.status === "available"
                  ? `${appVersion} → ${update.version}`
                  : updateLabel(update)}
              </div>
            </div>
            <button
              onClick={onAppUpdate}
              disabled={appWorking || !appActionable}
              className="shrink-0 rounded-md border border-accent/50 bg-accent/10 px-3 py-1.5 text-xs font-medium text-accent
                         transition-colors hover:bg-accent/20 disabled:opacity-50
                         focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
            >
              {update.status === "downloading"
                ? "Downloading…"
                : update.status === "ready"
                  ? "Restarting…"
                  : update.status === "error"
                    ? "Retry"
                    : "Update"}
            </button>
          </div>
        )}

        {/* Motore video: l'unico sidecar aggiornabile */}
        {engine?.updateAvailable && (
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="text-sm font-medium">Video engine</div>
              <div className="truncate font-mono text-xs text-ink-dim">
                {engine.name} {engine.current} → {engine.latest}
              </div>
            </div>
            <button
              onClick={onEngineUpdate}
              disabled={engineBusy}
              className="shrink-0 rounded-md border border-accent/50 bg-accent/10 px-3 py-1.5 text-xs font-medium text-accent
                         transition-colors hover:bg-accent/20 disabled:opacity-50
                         focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
            >
              {engineBusy ? "Updating…" : "Update"}
            </button>
          </div>
        )}

        {engineMsg && (
          <p className="border-t border-line pt-2 text-xs text-ink-dim">
            {engineMsg}
          </p>
        )}
        <p className="border-t border-line pt-2 text-xs text-ink-dim">
          Keep the video engine up to date: when sites change, an old version
          stops working.
        </p>
      </div>
    </div>
  );
}

const VIDEO_MODES: [VideoMode, string][] = [
  ["full", "Video + audio"],
  ["videoOnly", "Video only"],
  ["audioOnly", "Audio only"],
];

// --- Formattatori per le metriche live della scheda ---
function fmtDuration(sec?: number): string | null {
  if (sec == null || !isFinite(sec)) return null;
  const s = Math.round(sec);
  const m = Math.floor(s / 60);
  const h = Math.floor(m / 60);
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m % 60)}:${pad(s % 60)}` : `${m}:${pad(s % 60)}`;
}
function fmtBytes(b?: number): string | null {
  if (b == null || !isFinite(b)) return null;
  if (b < 1024) return `${b} B`;
  const kb = b / 1024;
  if (kb < 1024) return `${kb.toFixed(0)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}
function fmtSpeed(bps?: number): string | null {
  const s = fmtBytes(bps);
  return s ? `${s}/s` : null;
}
function fmtEta(sec?: number): string | null {
  if (sec == null || !isFinite(sec)) return null;
  const s = Math.round(sec);
  if (s < 60) return `~${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `~${m}m ${s % 60}s`;
  return `~${Math.floor(m / 60)}h ${m % 60}m`;
}
// Data di una voce di cronologia: "oggi 14:32", "ieri 09:15", "11 lug, 14:32"
function fmtWhen(epochSec: number): string {
  const d = new Date(epochSec * 1000);
  const time = d.toLocaleTimeString("en-US", {
    hour: "2-digit",
    minute: "2-digit",
  });
  const today = new Date();
  const dayMs = 24 * 60 * 60 * 1000;
  const startOf = (x: Date) =>
    new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diffDays = Math.round((startOf(today) - startOf(d)) / dayMs);
  if (diffDays === 0) return `today ${time}`;
  if (diffDays === 1) return `yesterday ${time}`;
  const date = d.toLocaleDateString("en-US", { day: "numeric", month: "short" });
  return `${date}, ${time}`;
}

// Badge di stato in alto a destra sulla scheda
function StatusBadge({ status }: { status: TimelineItem["status"] }) {
  const map: Record<string, [string, string]> = {
    pending: ["Queued", "text-ink-dim bg-panel-2 border border-line"],
    running: ["Running", "text-accent bg-accent/12"],
    ok: ["Done", "text-ok bg-ok/12"],
    failed: ["Failed", "text-err bg-err/12"],
    nothing: ["Empty", "text-ink-dim bg-panel-2"],
  };
  const [label, cls] = map[status] ?? map.pending;
  return (
    <span
      className={
        "shrink-0 rounded-full px-2 py-0.5 font-mono text-[10px] font-semibold uppercase tracking-wide " +
        cls
      }
    >
      {label}
    </span>
  );
}

// Una scheda per elemento della coda (variante B). Mostra anteprima + stato
// proprio: attesa / analisi (skeleton + fase che evolve) / download (barra +
// velocità/ETA/dimensione) / finito / errore.
function TaskCard({
  t,
  canAct,
  onRetry,
  onRemove,
}: {
  t: TimelineItem;
  canAct: boolean;
  onRetry: () => void;
  onRemove: () => void;
}) {
  const isVideo = t.engine === "video";
  const running = t.status === "running";
  const analyzing = running && (t.percent == null || t.percent === undefined);
  const hasThumb = !!t.thumbnail;
  const dur = fmtDuration(t.duration);
  // Nome mostrato: titolo se disponibile (video), altrimenti l'URL incollato
  const name = t.title ?? t.url;
  const [copied, setCopied] = useState(false);
  // Azioni disponibili solo a coda ferma e per elementi non più in lavorazione
  const done = ["ok", "failed", "nothing"].includes(t.status);
  const canRetry = canAct && (t.status === "failed" || t.status === "nothing");
  const showActions = canAct && done;

  async function copyLink() {
    try {
      await navigator.clipboard.writeText(t.url);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard non disponibile: ignora */
    }
  }

  return (
    <div
      className={
        "grid grid-cols-[6.5rem_1fr] gap-3 rounded-lg border bg-panel-2 p-3 " +
        (running ? "border-accent/40" : "border-line")
      }
    >
      {/* Miniatura / placeholder */}
      <div className="relative h-[3.7rem] w-[6.5rem] shrink-0 overflow-hidden rounded-md bg-panel">
        {hasThumb ? (
          <img
            src={t.thumbnail}
            alt=""
            className="size-full object-cover"
            loading="lazy"
          />
        ) : analyzing && isVideo ? (
          <div className="skeleton size-full" />
        ) : (
          <div className="flex size-full items-center justify-center text-ink-faint">
            {isVideo ? (
              <span aria-hidden className="text-lg">▶</span>
            ) : (
              <span aria-hidden className="text-lg">▨</span>
            )}
          </div>
        )}
        {dur && (
          <span className="absolute bottom-1 right-1 rounded bg-black/70 px-1 font-mono text-[10px] text-white">
            {dur}
          </span>
        )}
      </div>

      {/* Testo + stato */}
      <div className="flex min-w-0 flex-col justify-center gap-1">
        <div className="flex items-center gap-2">
          <span
            className={
              "min-w-0 flex-1 truncate text-sm font-medium " +
              (t.title ? "text-ink" : "font-mono text-xs text-ink-dim")
            }
            title={name}
          >
            {name}
          </span>
          <StatusBadge status={t.status} />
        </div>

        {/* Sotto-riga che cambia con lo stato */}
        {analyzing ? (
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center gap-2 text-xs text-ink-dim">
              <span className="live-dot" aria-hidden />
              <span className="truncate">{t.phase ?? "Analyzing…"}</span>
            </div>
            <div
              className="progress-indeterminate"
              role="progressbar"
              aria-label="Analyzing"
            />
          </div>
        ) : running ? (
          <div className="flex flex-col gap-1.5">
            <div
              className="h-[5px] overflow-hidden rounded-full bg-line"
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round(t.percent ?? 0)}
            >
              <div
                className="h-full rounded-full bg-gradient-to-r from-accent to-accent-strong transition-[width] duration-300"
                style={{ width: `${t.percent ?? 0}%` }}
              />
            </div>
            <div className="flex flex-wrap gap-x-3 gap-y-0.5 font-mono text-[11px] tabular-nums text-ink-dim">
              <span>{Math.round(t.percent ?? 0)}%</span>
              {fmtSpeed(t.speed) && (
                <span className="text-accent-soft">{fmtSpeed(t.speed)}</span>
              )}
              {fmtEta(t.eta) && <span>{fmtEta(t.eta)}</span>}
              {fmtBytes(t.downloaded) && fmtBytes(t.total) && (
                <span>
                  {fmtBytes(t.downloaded)} / {fmtBytes(t.total)}
                </span>
              )}
            </div>
          </div>
        ) : t.status === "failed" ? (
          <span className="text-xs text-err/90">
            {t.reason ?? "Couldn't download this link"}
          </span>
        ) : t.status === "nothing" ? (
          <span className="text-xs text-ink-dim">
            Nothing to download for the selected type
          </span>
        ) : t.status === "ok" ? (
          <span className="text-xs text-ink-dim">
            {[
              isVideo ? "Video" : "Images",
              t.uploader,
              fmtBytes(t.total) ?? undefined,
            ]
              .filter(Boolean)
              .join(" · ")}
          </span>
        ) : (
          <span className="text-xs text-ink-faint">Waiting…</span>
        )}

        {/* Azioni per elemento (solo a coda ferma):
            Riprova / Mostra file / Apri cartella / Copia / Rimuovi */}
        {showActions && (
          <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
            {canRetry && (
              <button
                onClick={onRetry}
                className="whitespace-nowrap font-medium text-accent hover:text-accent-strong
                           focus:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded"
              >
                Retry
              </button>
            )}
            {/* L'opener apre Esplora risorse sul PC: ha senso solo nell'app */}
            {!isRemote && t.status === "ok" && t.filePath && (
              <button
                onClick={() => revealFile(t.filePath!).catch(() => {})}
                className="whitespace-nowrap text-ink-dim hover:text-ink
                           focus:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded"
              >
                Show file
              </button>
            )}
            {!isRemote && t.status === "ok" && t.dir && (
              <button
                onClick={() => openFolder(t.dir!).catch(() => {})}
                className="whitespace-nowrap text-ink-dim hover:text-ink
                           focus:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded"
              >
                Open folder
              </button>
            )}
            <button
              onClick={copyLink}
              className="whitespace-nowrap text-ink-dim hover:text-ink
                         focus:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded"
            >
              {copied ? "Copied ✓" : "Copy link"}
            </button>
            <button
              onClick={onRemove}
              className="ml-auto text-ink-dim hover:text-err
                         focus:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded"
            >
              Remove
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

// Una riga della cronologia: voce statica (task concluso in una sessione
// passata o in questa), con le azioni per ritrovare i file o riscaricare.
// Per i task immagini (forum) non c'è titolo né miniatura: regola privacy.
function HistoryRow({
  e,
  busy,
  onRedownload,
}: {
  e: HistoryEntry;
  busy: boolean;
  onRedownload: () => void;
}) {
  const isVideo = e.engine === "video";
  const name = e.title ?? e.url;
  const [copied, setCopied] = useState(false);

  async function copyLink() {
    try {
      await navigator.clipboard.writeText(e.url);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard non disponibile: ignora */
    }
  }

  return (
    <div className="grid grid-cols-[4.5rem_1fr] gap-3 rounded-lg border border-line bg-panel-2 p-3">
      {/* Miniatura (solo video con anteprima) o segnaposto */}
      <div className="relative h-[2.6rem] w-[4.5rem] shrink-0 overflow-hidden rounded-md bg-panel">
        {e.thumbnail ? (
          <img
            src={e.thumbnail}
            alt=""
            className="size-full object-cover"
            loading="lazy"
          />
        ) : (
          <div className="flex size-full items-center justify-center text-ink-faint">
            <span aria-hidden className="text-base">
              {isVideo ? "▶" : "▨"}
            </span>
          </div>
        )}
      </div>

      <div className="flex min-w-0 flex-col justify-center gap-1">
        <div className="flex items-center gap-2">
          <span
            className={
              "min-w-0 flex-1 truncate text-sm font-medium " +
              (e.title ? "text-ink" : "font-mono text-xs text-ink-dim")
            }
            title={name}
          >
            {name}
          </span>
          <StatusBadge status={e.outcome} />
        </div>
        <span className="text-xs text-ink-dim">
          {[fmtWhen(e.when), isVideo ? "Video" : "Images", e.uploader]
            .filter(Boolean)
            .join(" · ")}
        </span>
        {e.outcome === "failed" && e.reason && (
          <span className="text-xs text-err/90">{e.reason}</span>
        )}

        <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
          <button
            onClick={onRedownload}
            disabled={busy}
            className="whitespace-nowrap font-medium text-accent hover:text-accent-strong disabled:opacity-40
                       focus:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded"
          >
            Download again
          </button>
          {e.outcome === "ok" && e.filePath && (
            <button
              onClick={() => revealFile(e.filePath!).catch(() => {})}
              className="whitespace-nowrap text-ink-dim hover:text-ink
                         focus:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded"
            >
              Show file
            </button>
          )}
          {e.dir && (
            <button
              onClick={() => openFolder(e.dir).catch(() => {})}
              className="whitespace-nowrap text-ink-dim hover:text-ink
                         focus:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded"
            >
              Open folder
            </button>
          )}
          <button
            onClick={copyLink}
            className="whitespace-nowrap text-ink-dim hover:text-ink
                       focus:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded"
          >
            {copied ? "Copied ✓" : "Copy link"}
          </button>
        </div>
      </div>
    </div>
  );
}

// mp3/wav riconvertiti per compatibilità con gli editor (DaVinci ecc.);
// opus = nessuna riconversione, qualità originale di YouTube & co.
const AUDIO_FORMATS: [AudioFormat, string][] = [
  ["mp3", "MP3"],
  ["wav", "WAV"],
  ["opus", "Opus (original)"],
];

// Tetto di risoluzione (yt-dlp -S "res:N"): prende il migliore FINO a quel
// valore. 0 = massima disponibile (comportamento storico).
const RESOLUTIONS: [number, string][] = [
  [0, "Max"],
  [1080, "1080p"],
  [720, "720p"],
  [480, "480p"],
];

// Contenitore/codec del video. "mp4" e "editing" NON ricodificano (remux:
// qualità identica, cambia solo la scatola); "editing" in più sceglie la
// traccia H.264, l'unica che DaVinci/Premiere aprono senza sorprese.
const VIDEO_FORMATS: [VideoFormat, string][] = [
  ["auto", "Original"],
  ["mp4", "MP4"],
  ["editing", "MP4 for editing (H.264)"],
];

// Sottotitoli: nessuno / dentro al video / file .srt separato / entrambi.
// Lingue automatiche (inglese + italiano se ci sono), nessuna scelta lingua.
const SUBS_MODES: [SubsMode, string][] = [
  ["no", "None"],
  ["embed", "In the video"],
  ["file", ".srt file"],
  ["both", "Both"],
];

// Frammenti video paralleli (yt-dlp -N). Più flussi = più veloce su fibra;
// su rame il guadagno è minore. Valori oltre 8 danno rese marginali.
const SPEED_LEVELS: [number, string][] = [
  [1, "Normal"],
  [4, "Fast"],
  [8, "Turbo"],
];

// Solo Firefox: i browser Chromium (Chrome/Edge/Brave) da Chrome 127 blindano
// i cookie e sono illeggibili dai programmi esterni, quindi non li offriamo.
const COOKIE_BROWSERS: [CookiesBrowser, string][] = [
  ["", "None"],
  ["firefox", "Firefox"],
];

const inputCls =
  "w-full rounded-md border border-line bg-panel-2 px-3 py-2 text-sm text-ink " +
  "placeholder:text-ink-dim focus:outline-none focus-visible:ring-2 focus-visible:ring-accent";

const sectionTitleCls =
  "font-mono text-xs font-semibold uppercase tracking-widest text-ink-dim";

export default function App() {
  const [bulk, setBulk] = useState(false);

  // Modalità normale: uno o più link, uno per riga
  const [singleUrls, setSingleUrls] = useState("");

  // Modalità bulk: link di esempio + numero da far variare
  const [exampleUrl, setExampleUrl] = useState("");
  const [selIdx, setSelIdx] = useState<number | null>(null);
  const [from, setFrom] = useState(1);
  const [to, setTo] = useState(9);
  const [zeroPad, setZeroPad] = useState(false);

  const [wantVideo, setWantVideo] = useState(true);
  const [wantImages, setWantImages] = useState(false);
  const [videoMode, setVideoMode] = useState<VideoMode>("full");
  // Tetto di risoluzione e contenitore/codec; persistiti come le altre preferenze
  const [maxHeight, setMaxHeight] = useState<number>(
    () => Number(localStorage.getItem("maxHeight")) || 0,
  );
  useEffect(() => {
    localStorage.setItem("maxHeight", String(maxHeight));
  }, [maxHeight]);
  const [videoFormat, setVideoFormat] = useState<VideoFormat>(
    () => (localStorage.getItem("videoFormat") as VideoFormat) ?? "auto",
  );
  useEffect(() => {
    localStorage.setItem("videoFormat", videoFormat);
  }, [videoFormat]);
  // Sottotitoli: scelta per-download (accanto alle tracce), default "No"
  const [subs, setSubs] = useState<SubsMode>(
    () => (localStorage.getItem("subs") as SubsMode) ?? "no",
  );
  useEffect(() => {
    localStorage.setItem("subs", subs);
  }, [subs]);
  // Formato della modalità "solo audio"; persistito come le altre preferenze
  const [audioFormat, setAudioFormat] = useState<AudioFormat>(
    () => (localStorage.getItem("audioFormat") as AudioFormat) ?? "mp3",
  );
  useEffect(() => {
    localStorage.setItem("audioFormat", audioFormat);
  }, [audioFormat]);
  // Frammenti video paralleli (yt-dlp -N): 1 normale, 4 veloce, 8 turbo.
  // Su fibra più flussi saturano la linea; su rame il guadagno è minore.
  const [concurrency, setConcurrency] = useState<number>(
    () => Number(localStorage.getItem("concurrency")) || 4,
  );
  useEffect(() => {
    localStorage.setItem("concurrency", String(concurrency));
  }, [concurrency]);
  // Login preso in prestito dal browser (per i siti che lo richiedono); persistito
  const [cookies, setCookies] = useState<CookiesBrowser>(
    () => (localStorage.getItem("cookiesBrowser") as CookiesBrowser) ?? "",
  );
  useEffect(() => {
    localStorage.setItem("cookiesBrowser", cookies);
  }, [cookies]);
  // La coda corrente (o l'ultima completata), mostrata come timeline
  const [timeline, setTimeline] = useState<TimelineItem[]>([]);
  // La cartella scelta viene ricordata tra un avvio e l'altro dell'app
  const [outputDir, setOutputDir] = useState(
    () => localStorage.getItem("outputDir") ?? "",
  );
  useEffect(() => {
    localStorage.setItem("outputDir", outputDir);
  }, [outputDir]);
  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState(false);

  // Solo dal telefono: il collegamento col PC è caduto (schermo bloccato,
  // Wi-Fi che cambia…). Si riconnette da solo; intanto lo diciamo, perché
  // altrimenti la coda sembrerebbe bloccata mentre sul PC va avanti.
  const [linkDown, setLinkDown] = useState(false);
  // Motori spariti (di solito: antivirus). Controllato all'avvio.
  const [missingEng, setMissingEng] = useState<string[]>([]);
  // Modalità server: gestione PIN e dati per il pannello "Accesso dal telefono"
  const [pinNeeded, setPinNeeded] = useState(false);
  const [pinInput, setPinInput] = useState(getPin());
  const [authTick, setAuthTick] = useState(0);
  const [srv, setSrv] = useState<ServerInfo | null>(null);
  // Vista corrente (solo desktop): Media, Cronologia, Remote o Impostazioni
  const [view, setView] = useState<
    "download" | "history" | "remote" | "settings"
  >("download");
  // Cronologia persistente (null = non ancora caricata)
  const [history, setHistory] = useState<HistoryEntry[] | null>(null);
  // Coda rimasta a metà nella sessione precedente (banner Riprendi/Scarta)
  const [interrupted, setInterrupted] = useState<InterruptedQueue | null>(null);
  // Selettore cartelle: naviga il disco del PC dal telefono
  const [browser, setBrowser] = useState<DirListing | null>(null);
  const [browserBusy, setBrowserBusy] = useState(false);
  const [newFolder, setNewFolder] = useState<string | null>(null); // null = form chiuso
  // Stabile: useModal la usa come dipendenza, una funzione nuova a ogni render
  // rimonterebbe il gestore dei tasti a ogni giro.
  const closeBrowser = useCallback(() => {
    setBrowser(null);
    setNewFolder(null);
  }, []);

  // Aggiorna un solo elemento della timeline (helper per gli eventi per-indice)
  const patchItem = (index: number, patch: Partial<TimelineItem>) =>
    setTimeline((tl) => tl.map((t, i) => (i === index ? { ...t, ...patch } : t)));

  function handleEvent(ev: Parameters<Parameters<typeof onDownloadEvent>[0]>[0]) {
    switch (ev.kind) {
      case "queueStart":
        setTimeline(
          ev.tasks.map((t) => ({ ...t, status: "pending" as const })),
        );
        setBusy(true);
        // Una coda nuova rende obsoleto il banner della coda interrotta
        // (è anche la strada della ripresa stessa: Riprendi → queueStart)
        setInterrupted(null);
        break;
      case "queueAppend":
        // Riprova: accoda i nuovi task in fondo alla timeline esistente
        setTimeline((tl) => [
          ...tl,
          ...ev.tasks.map((t) => ({ ...t, status: "pending" as const })),
        ]);
        setBusy(true);
        break;
      case "itemStart":
        // Nuovo elemento in lavorazione: azzera lo stato live e apri in "analisi"
        patchItem(ev.index, {
          status: "running",
          phase: "Starting…",
          percent: null,
          speed: undefined,
          eta: undefined,
          downloaded: undefined,
          total: undefined,
        });
        break;
      case "preview":
        patchItem(ev.index, {
          title: ev.title,
          uploader: ev.uploader,
          duration: ev.duration,
          thumbnail: ev.thumbnail,
        });
        break;
      case "phase":
        patchItem(ev.index, { phase: ev.phase });
        break;
      case "progress":
        // Arrivato il primo dato di download: la fase d'analisi è finita.
        // La velocità grezza oscilla molto: la smorzo con una media esponenziale
        // (EMA) così il numero non "balla" ad ogni tick.
        setTimeline((tl) =>
          tl.map((t, i) => {
            if (i !== ev.index) return t;
            const speed =
              ev.speed == null
                ? undefined
                : t.speed == null
                  ? ev.speed
                  : t.speed + 0.3 * (ev.speed - t.speed);
            return {
              ...t,
              phase: undefined,
              percent: ev.percent,
              speed,
              eta: ev.eta,
              downloaded: ev.downloaded,
              total: ev.total,
            };
          }),
        );
        break;
      case "line":
        // Righe neutre residue (es. "Unisco audio e video…"): le mostro come fase
        patchItem(ev.index, { phase: ev.line });
        break;
      case "itemDone":
        patchItem(ev.index, {
          status: ev.outcome,
          reason: ev.reason,
          dir: ev.dir,
          filePath: ev.filePath,
          phase: undefined,
        });
        break;
      case "finished": {
        setBusy(false);
        const parts = [`${ev.ok} completed`];
        if (ev.failed > 0) parts.push(`${ev.failed} failed`);
        if (ev.nothing > 0)
          parts.push(`${ev.nothing} with no content for the selected engine`);
        setStatus((ev.cancelled ? "Cancelled — " : "Finished: ") + parts.join(", "));
        // Notifica di sistema a coda finita (l'app spesso vive nel tray).
        // Non su annullo: l'ha chiesto l'utente, lo sa già. Letta da
        // localStorage perché questa closure è registrata una volta sola.
        if (!ev.cancelled && localStorage.getItem("notifyFinish") !== "0")
          notify("Harvest — queue completed", parts.join(", "));
        // I task mai partiti restano "in attesa": dopo un annullo li segno come tali
        if (ev.cancelled)
          setTimeline((tl) =>
            tl.map((t) =>
              t.status === "pending" || t.status === "running"
                ? { ...t, status: "failed", reason: "Cancelled", phase: undefined }
                : t,
            ),
          );
        break;
      }
    }
  }

  useEffect(() => {
    let dead = false;
    let unlisten: (() => void) | undefined;

    // Ricarica dal PC la fotografia della coda. Serve all'avvio e a OGNI
    // riconnessione: gli eventi persi mentre il telefono era scollegato non
    // tornano indietro, quindi senza questo la schermata resterebbe ferma a
    // uno stato vecchio.
    async function syncState(): Promise<boolean> {
      try {
        const s = await fetchServerState();
        if (dead) return false;
        setPinNeeded(false);
        setBusy(s.busy);
        setTimeline(
          s.timeline.map((t) => ({
            url: t.url,
            engine: t.engine,
            status: t.status as TimelineItem["status"],
            reason: t.reason,
            title: t.title,
            uploader: t.uploader,
            duration: t.duration,
            thumbnail: t.thumbnail,
          })),
        );
        setOutputDir((d) => d || s.lastOutputDir);
        return true;
      } catch (e) {
        if (!dead && e instanceof PinError) setPinNeeded(true);
        else if (!dead) setStatus(`Connection to the PC failed: ${e}`);
        return false;
      }
    }

    async function init() {
      // Dal telefono: prima la fotografia della coda (e la verifica del PIN)...
      if (isRemote && !(await syncState())) return;
      // ...poi gli eventi in tempo reale (nell'app: da subito)
      const un = await onDownloadEvent(handleEvent, (connected) => {
        if (dead) return;
        setLinkDown(!connected);
        // Tornati online: rimettiamoci in pari con quello che è successo
        if (connected) void syncState();
      });
      if (dead) un();
      else unlisten = un;
      // Coda rimasta a metà nella sessione precedente? Mostra il banner.
      getInterrupted()
        .then((q) => {
          if (!dead) setInterrupted(q);
        })
        .catch(() => {});
    }
    init();

    return () => {
      dead = true;
      unlisten?.();
    };
  }, [authTick]);

  // Nell'app desktop: dati per il pannello "Accesso dal telefono" + autostart
  const [autostart, setAutostartState] = useState(false);
  const [update, setUpdate] = useState<UpdateState>({ status: "idle" });
  // Notifica di sistema a fine coda (Impostazioni); default attiva.
  // handleEvent la legge da localStorage (non da qui) per evitare closure stantie.
  const [notifyFinish, setNotifyFinish] = useState(
    () => localStorage.getItem("notifyFinish") !== "0",
  );
  useEffect(() => {
    localStorage.setItem("notifyFinish", notifyFinish ? "1" : "0");
  }, [notifyFinish]);
  // Arricchimento file (tag, copertina, capitoli): preferenza in Impostazioni,
  // default attivo. Migliora tutti i file (audio e video), quindi ON di default.
  const [enrich, setEnrich] = useState(
    () => localStorage.getItem("enrich") !== "0",
  );
  useEffect(() => {
    localStorage.setItem("enrich", enrich ? "1" : "0");
  }, [enrich]);
  const [appVersion, setAppVersion] = useState("");
  // Motore video (yt-dlp): l'unico aggiornabile. Il controllo all'avvio serve
  // alla pillola: senza chiedere "c'è una versione nuova?" non potrebbe comparire.
  const [engine, setEngine] = useState<EngineInfo | null>(null);
  const [engineBusy, setEngineBusy] = useState(false);
  const [engineMsg, setEngineMsg] = useState("");
  // Pannellino aperto dalla pillola di aggiornamento
  const [updatesOpen, setUpdatesOpen] = useState(false);
  // Stabile: dipendenza di useModal dentro UpdatesPanel (vedi closeBrowser)
  const closeUpdates = useCallback(() => setUpdatesOpen(false), []);
  useEffect(() => {
    if (!isRemote) {
      fetchServerInfo().then(setSrv).catch(() => {});
      getAutostart().then(setAutostartState).catch(() => {});
      getAppVersion().then(setAppVersion).catch(() => {});
      // Controllo aggiornamenti in background all'avvio (SOLO check, non scarica):
      // se c'è una novità, il badge compare nell'header. Silenzioso se non c'è nulla.
      checkUpdate(setUpdate).catch(() => {});
      // Idem per il motore: solo il confronto delle versioni, nessun download.
      checkEngine().then(setEngine).catch(() => {});
      // Un motore sparito (antivirus)? Meglio dirlo ora che al primo download.
      missingEngines().then(setMissingEng).catch(() => {});
    }
  }, []);

  /** Aggiorna il motore video e rinfresca le versioni mostrate. */
  const doUpdateEngine = async () => {
    setEngineBusy(true);
    setEngineMsg("");
    try {
      const v = await updateEngine();
      setEngineMsg(`Updated to version ${v}`);
      setEngine(await checkEngine());
    } catch (e) {
      setEngineMsg(String(e));
    } finally {
      setEngineBusy(false);
    }
  };

  // La pillola compare solo se c'è davvero qualcosa da fare o da dire.
  const appUpdate =
    update.status !== "idle" && update.status !== "none" ? update : null;
  const engineUpdate = engine?.updateAvailable ? engine : null;
  const hasUpdates = !isRemote && (appUpdate !== null || engineUpdate !== null);

  // La cronologia si carica quando si apre la tab (e si ricarica a ogni
  // ritorno: possono esserci task conclusi nel frattempo). Più recenti in cima.
  useEffect(() => {
    if (view === "history") {
      getHistory()
        .then((h) => setHistory(h.slice().reverse()))
        .catch(() => setHistory([]));
    }
  }, [view]);

  async function onClearHistory() {
    try {
      if (!(await askConfirm("Clear the entire history?"))) return;
      await clearHistory();
      setHistory([]);
    } catch (err) {
      setStatus(`${err}`);
    }
  }

  // Riscarica una voce di cronologia: stessa logica di Riprova (si accoda
  // alla timeline corrente), poi si passa alla vista Media per vederla.
  async function onRedownload(e: HistoryEntry) {
    if (busy) return;
    const dir = outputDir || e.dir;
    if (!dir) return;
    setBusy(true);
    setStatus("");
    setView("download");
    try {
      await startDownload(
        [e.url],
        e.engine === "video",
        e.engine === "images",
        videoMode,
        videoFormat,
        maxHeight,
        audioFormat,
        enrich,
        subs,
        concurrency,
        cookies,
        dir,
        true, // append
      );
    } catch (err) {
      setBusy(false);
      setStatus(`Error: ${err}`);
    }
  }

  async function onResume() {
    setStatus("");
    try {
      await resumeQueue();
      // Il resto arriva dagli eventi (queueStart toglie anche il banner)
    } catch (err) {
      setStatus(`${err}`);
    }
  }

  async function onDiscard() {
    try {
      await discardQueue();
      setInterrupted(null);
    } catch (err) {
      setStatus(`${err}`);
    }
  }

  async function toggleAutostart(enabled: boolean) {
    setAutostartState(enabled); // ottimistico
    try {
      await setAutostart(enabled);
    } catch {
      setAutostartState(!enabled); // ripristina se fallisce
    }
  }

  const numbers = useMemo(() => findNumbers(exampleUrl.trim()), [exampleUrl]);
  // Di default si assume che a variare sia l'ultimo numero dell'URL (di solito è la pagina)
  const selected =
    numbers.length > 0
      ? numbers[Math.min(selIdx ?? numbers.length - 1, numbers.length - 1)]
      : null;

  // Quando cambia il numero scelto, Da e Zeri iniziali si allineano all'esempio
  useEffect(() => {
    if (!selected) return;
    setFrom(parseInt(selected.text, 10));
    setZeroPad(selected.text.length > 1 && selected.text.startsWith("0"));
  }, [selected?.start, selected?.text]);

  const seriesUrls = useMemo(
    () =>
      selected
        ? generateSeriesUrls(exampleUrl.trim(), selected, from, to, zeroPad)
        : [],
    [exampleUrl, selected, from, to, zeroPad],
  );
  // Quanti link chiede l'intervallo (anche oltre il tetto): serve per dire
  // all'utente *quanti* sono, non solo che sono troppi.
  const seriesTotal = selected ? seriesCount(from, to) : 0;
  const tooManySeries = seriesTotal > MAX_SERIES;

  const links = bulk
    ? seriesUrls
    : singleUrls.split("\n").map((l) => l.trim()).filter(Boolean);

  const canStart =
    links.length > 0 && outputDir !== "" && (wantVideo || wantImages) && !busy;

  // Download falliti, con il link e il motivo: alimentano il pannello Errori.
  // Solo a coda ferma (a download in corso la lista è ancora incompleta).
  const failedItems = useMemo(
    () => timeline.filter((t) => t.status === "failed"),
    [timeline],
  );

  async function onBrowse() {
    const folder = await pickOutputFolder();
    if (folder) setOutputDir(folder);
  }

  async function openFolderPicker() {
    setBrowserBusy(true);
    try {
      setBrowser(await browseDir(outputDir || null));
    } catch (e) {
      setStatus(`Couldn't read the folders: ${e}`);
    } finally {
      setBrowserBusy(false);
    }
  }

  async function browseTo(path: string | null) {
    setBrowserBusy(true);
    setNewFolder(null);
    try {
      setBrowser(await browseDir(path));
    } finally {
      setBrowserBusy(false);
    }
  }

  async function confirmNewFolder() {
    if (!browser?.path || !newFolder?.trim()) return;
    setBrowserBusy(true);
    try {
      setBrowser(await createDir(browser.path, newFolder.trim()));
      setNewFolder(null);
    } catch (e) {
      setStatus(`${e}`);
    } finally {
      setBrowserBusy(false);
    }
  }

  async function onStart() {
    setBusy(true);
    setStatus("");
    try {
      await startDownload(
        links,
        wantVideo,
        wantImages,
        videoMode,
        videoFormat,
        maxHeight,
        audioFormat,
        enrich,
        subs,
        concurrency,
        cookies,
        outputDir,
      );
      // Da qui in poi lo stato arriva dagli eventi del backend (fino a "finished")
    } catch (err) {
      setBusy(false);
      setStatus(`Error: ${err}`);
    }
  }

  // Ctrl+Invio avvia la coda da qualunque punto (anche dal campo dei link,
  // dove il solo Invio serve ad andare a capo). Non scatta se c'è una finestra
  // aperta: lì l'Invio ha già un significato suo (es. creare la cartella).
  // NB: Esc NON annulla il download — è un tasto che si preme d'istinto per
  // "chiudi", e butterebbe via una coda in corso. Chiude solo le finestre
  // (vedi useModal). Per annullare c'è il bottone.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Enter" || !(e.ctrlKey || e.metaKey)) return;
      if (browser || updatesOpen || !canStart) return;
      e.preventDefault();
      void onStart();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
    // Senza array di dipendenze di proposito: il gestore deve sempre vedere i
    // link e canStart aggiornati, altrimenti resterebbe a uno stato vecchio.
  });

  // Riprova un singolo elemento (fallito/vuoto): lo riscarica accodandolo alla
  // timeline. Il tipo (video/immagini) è quello dell'elemento, non delle
  // checkbox correnti. Solo a coda ferma.
  async function onRetry(item: TimelineItem) {
    if (busy || !outputDir) return;
    setBusy(true);
    setStatus("");
    try {
      await startDownload(
        [item.url],
        item.engine === "video",
        item.engine === "images",
        videoMode,
        videoFormat,
        maxHeight,
        audioFormat,
        enrich,
        subs,
        concurrency,
        cookies,
        outputDir,
        true, // append
      );
    } catch (err) {
      setBusy(false);
      setStatus(`Error: ${err}`);
    }
  }

  // Rimuove un elemento dalla coda (solo a coda ferma). Aggiorna backend e UI.
  async function onRemove(index: number) {
    if (busy) return;
    try {
      await removeItem(index);
      setTimeline((tl) => tl.filter((_, i) => i !== index));
    } catch (err) {
      setStatus(`${err}`);
    }
  }

  // L'URL di esempio reso come testo + numeri cliccabili
  function renderSegmentedUrl() {
    const url = exampleUrl.trim();
    const parts: ReactNode[] = [];
    let pos = 0;
    numbers.forEach((m, i) => {
      if (m.start > pos)
        parts.push(<span key={`t${i}`}>{url.slice(pos, m.start)}</span>);
      const isSel = selected != null && m.start === selected.start;
      parts.push(
        <button
          key={`n${i}`}
          onClick={() => setSelIdx(i)}
          aria-pressed={isSel}
          className={
            "rounded px-1 font-semibold focus:outline-none focus-visible:ring-2 focus-visible:ring-accent " +
            (isSel
              ? "bg-accent text-accent-ink"
              : "bg-panel-2 text-ink hover:bg-line")
          }
        >
          {m.text}
        </button>,
      );
      pos = m.start + m.text.length;
    });
    if (pos < url.length) parts.push(<span key="end">{url.slice(pos)}</span>);
    return parts;
  }

  // Dal telefono, senza PIN valido: solo la schermata di accesso
  if (pinNeeded) {
    return (
      <div className="flex min-h-screen items-center justify-center px-6">
        <form
          onSubmit={(e) => {
            e.preventDefault();
            savePin(pinInput.trim());
            setAuthTick((t) => t + 1);
          }}
          className="flex w-full max-w-xs flex-col gap-4 rounded-lg border border-line bg-panel p-6"
        >
          <h1 className="text-2xl font-bold tracking-tight">
            Harvest<span className="text-accent">.</span>
          </h1>
          <p className="text-sm text-ink-dim">
            Enter the PIN shown in the app on your PC, under the "Phone access"
            panel.
          </p>
          <input
            value={pinInput}
            onChange={(e) => setPinInput(e.target.value)}
            inputMode="numeric"
            maxLength={6}
            placeholder="123456"
            autoFocus
            className={inputCls + " text-center font-mono text-lg tracking-[0.5em]"}
          />
          <button
            type="submit"
            className="rounded-lg bg-accent py-2.5 text-base font-bold text-accent-ink transition-colors
                       hover:bg-accent-strong focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
          >
            Enter
          </button>
        </form>
      </div>
    );
  }

  return (
    <div className="mx-auto flex min-h-screen w-full max-w-6xl flex-col px-4 py-6 sm:px-8">
      {/* Solo dal telefono: il collegamento col PC è caduto. Senza questo
          avviso la coda sembrerebbe bloccata, mentre sul PC prosegue. */}
      {linkDown && (
        <div
          role="status"
          className="mb-4 flex items-center gap-2 rounded-lg border border-accent/40 bg-accent/10 px-4 py-2.5 text-sm text-accent"
        >
          <span className="size-2 animate-pulse rounded-full bg-accent" />
          Lost connection to the PC — retrying…
        </div>
      )}
      {/* Un motore non c'è più (di solito: antivirus che l'ha messo in
          quarantena). Va detto SUBITO: senza, l'app sembra a posto e fallisce
          solo al primo download, con un errore che non spiega niente. */}
      {missingEng.length > 0 && (
        <div
          role="alert"
          className="mb-4 rounded-lg border border-err/40 bg-err/10 px-4 py-3 text-sm text-err"
        >
          <strong className="font-semibold">
            A component is missing ({missingEng.join(", ")}).
          </strong>{" "}
          Usually your antivirus quarantined it: restore it, or reinstall
          Harvest. While it's missing, downloads won't work.
        </div>
      )}
      <header className="flex items-center justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">
            Harvest<span className="text-accent">.</span>
          </h1>
          <p className="text-sm text-ink-dim">
            Local video and image downloader
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-3">
          {!isRemote && (
            <nav className="flex gap-1 rounded-lg bg-panel p-1">
              {(
                [
                  ["download", "Media"],
                  ["history", "History"],
                  ["remote", "Remote"],
                ] as const
              ).map(([key, label]) => (
                <button
                  key={key}
                  onClick={() => setView(key)}
                  aria-current={view === key ? "page" : undefined}
                  className={
                    "rounded-md px-3 py-1.5 text-sm font-medium transition-colors " +
                    "focus:outline-none focus-visible:ring-2 focus-visible:ring-accent " +
                    (view === key
                      ? "bg-panel-2 text-ink"
                      : "text-ink-dim hover:text-ink")
                  }
                >
                  {label}
                </button>
              ))}
            </nav>
          )}
          {/* Impostazioni: ingranaggio fuori dalla navbar (è dell'app,
              non un modulo; sopravvive alla futura struttura a moduli) */}
          {!isRemote && (
            <button
              onClick={() => setView("settings")}
              aria-label="Settings"
              aria-current={view === "settings" ? "page" : undefined}
              title="Settings"
              className={
                "flex size-9 items-center justify-center rounded-lg transition-colors " +
                "focus:outline-none focus-visible:ring-2 focus-visible:ring-accent " +
                (view === "settings"
                  ? "bg-panel-2 text-ink"
                  : "bg-panel text-ink-dim hover:text-ink")
              }
            >
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="size-[18px]"
                aria-hidden
              >
                <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
                <circle cx="12" cy="12" r="3" />
              </svg>
            </button>
          )}
          {/* Aggiornamenti: una sola pillola per app e motore. Compare solo se
              c'è davvero qualcosa da aggiornare (o un esito da comunicare), così
              non è mai invadente; al clic apre il pannellino con le singole voci. */}
          {hasUpdates && (
            <div className="relative">
              <button
                onClick={() => setUpdatesOpen((o) => !o)}
                aria-expanded={updatesOpen}
                aria-haspopup="dialog"
                title="Updates available"
                className="flex items-center gap-2 rounded-full border border-accent/50 bg-accent/10 px-3 py-1.5 text-xs font-medium text-accent
                           transition-colors hover:bg-accent/20
                           focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
              >
                <span className="size-2 rounded-full bg-accent" />
                {appUpdate && engineUpdate
                  ? "Updates"
                  : engineUpdate
                    ? "Update engine"
                    : update.status === "downloading"
                      ? "Downloading..."
                      : update.status === "ready"
                        ? "Restarting..."
                        : update.status === "checking"
                          ? "Checking..."
                          : update.status === "error"
                            ? "Retry update"
                            : "Update now"}
              </button>
              {updatesOpen && (
                <UpdatesPanel
                  onClose={closeUpdates}
                  update={update}
                  appVersion={appVersion}
                  onAppUpdate={() => runUpdate(setUpdate)}
                  engine={engine}
                  engineBusy={engineBusy}
                  engineMsg={engineMsg}
                  onEngineUpdate={doUpdateEngine}
                />
              )}
            </div>
          )}
          <div
            className={
              "flex items-center gap-2 rounded-full border px-3 py-1.5 text-xs font-medium " +
              (busy
                ? "border-accent/50 bg-accent/10 text-accent"
                : "border-line bg-panel text-ink-dim")
            }
          >
            <span
              className={
                "size-2 rounded-full " +
                (busy ? "animate-pulse bg-accent" : "bg-ok")
              }
            />
            {busy ? "Working…" : "Ready"}
          </div>
        </div>
      </header>

      {view === "download" ? (
        <main className="mt-6 grid flex-1 items-start gap-5 lg:grid-cols-[minmax(0,26rem)_minmax(0,1fr)]">
        {/* Pannello sinistro: sorgente, opzioni, azioni */}
        <div className="flex flex-col gap-5">
          <section className="flex flex-col gap-4 rounded-lg border border-line bg-panel p-4">
            <div className="flex items-center justify-between">
              <h2 className={sectionTitleCls}>Source</h2>
              <label className="flex items-center gap-2 text-sm font-medium">
                <input
                  type="checkbox"
                  checked={bulk}
                  onChange={(e) => setBulk(e.target.checked)}
                  className="size-4 accent-(--color-accent)"
                />
                Bulk
              </label>
            </div>
            {!bulk ? (
              <label className="flex flex-col gap-1.5 text-sm">
                <span className="text-ink-dim">Links (one per line)</span>
                <textarea
                  value={singleUrls}
                  onChange={(e) => setSingleUrls(e.target.value)}
                  rows={5}
                  spellCheck={false}
                  placeholder={"https://example.com/video\nhttps://example.com/gallery"}
                  className={inputCls + " resize-y font-mono text-xs leading-5"}
                />
              </label>
            ) : (
              <>
                <label className="flex flex-col gap-1.5 text-sm">
                  <span className="text-ink-dim">
                    Paste an example link (e.g. the first page of the series)
                  </span>
                  <input
                    value={exampleUrl}
                    onChange={(e) => {
                      setExampleUrl(e.target.value);
                      setSelIdx(null);
                    }}
                    spellCheck={false}
                    placeholder="https://example.com/gallery/page-1"
                    className={inputCls + " font-mono text-xs"}
                  />
                </label>

                {exampleUrl.trim() !== "" && numbers.length === 0 && (
                  <p className="text-xs text-ink-dim">
                    The link has no numbers: a series can't be generated.
                  </p>
                )}

                {numbers.length > 0 && (
                  <div className="flex flex-col gap-1.5 text-sm">
                    <span className="text-ink-dim">
                      {numbers.length === 1
                        ? "Number that will change (detected automatically)"
                        : "Click the number that should change"}
                    </span>
                    <p className="break-all rounded-md border border-line bg-panel-2 px-3 py-2 font-mono text-xs leading-6">
                      {renderSegmentedUrl()}
                    </p>
                  </div>
                )}

                {selected && (
                  <>
                    <div className="flex flex-wrap items-end gap-4">
                      {/* Campo svuotato: valueAsNumber dà NaN, che come `value`
                          renderebbe l'input incontrollato (e mostrerebbe "NaN").
                          Lo teniamo vuoto: l'anteprima sparisce finché non c'è
                          un numero, ed è il comportamento atteso. */}
                      <label className="flex flex-col gap-1.5 text-sm">
                        <span className="text-ink-dim">From</span>
                        <input
                          type="number"
                          value={Number.isNaN(from) ? "" : from}
                          onChange={(e) => setFrom(e.target.valueAsNumber)}
                          className={inputCls + " w-24"}
                        />
                      </label>
                      <label className="flex flex-col gap-1.5 text-sm">
                        <span className="text-ink-dim">To</span>
                        <input
                          type="number"
                          value={Number.isNaN(to) ? "" : to}
                          onChange={(e) => setTo(e.target.valueAsNumber)}
                          className={inputCls + " w-24"}
                        />
                      </label>
                      <label className="flex items-center gap-2 py-2 text-sm">
                        <input
                          type="checkbox"
                          checked={zeroPad}
                          onChange={(e) => setZeroPad(e.target.checked)}
                          className="size-4 accent-(--color-accent)"
                        />
                        Leading zeros (e.g. 01)
                      </label>
                    </div>
                    {tooManySeries ? (
                      /* Oltre il tetto non generiamo nulla (l'app si
                         bloccherebbe): di solito è un errore di battitura. */
                      <p
                        role="alert"
                        className="rounded-md border border-err/40 bg-err/10 px-3 py-2 text-xs text-err"
                      >
                        That's {seriesTotal.toLocaleString("en-US")} links: too
                        many at once (max {MAX_SERIES}). Narrow the range.
                      </p>
                    ) : (
                      seriesUrls.length > 0 && (
                        <p className="break-all font-mono text-xs text-ink-dim">
                          {seriesUrls.length} links: {seriesUrls[0]}
                          {seriesUrls.length > 1 &&
                            ` ... ${seriesUrls[seriesUrls.length - 1]}`}
                        </p>
                      )
                    )}
                  </>
                )}
              </>
            )}
          </section>

          <section className="flex flex-col gap-4 rounded-lg border border-line bg-panel p-4">
            <h2 className={sectionTitleCls}>Options</h2>
            <div className="flex flex-col gap-1.5 text-sm">
              <span className="text-ink-dim">What to download</span>
              <div className="flex gap-6">
                <label className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={wantVideo}
                    onChange={(e) => setWantVideo(e.target.checked)}
                    className="size-4 accent-(--color-accent)"
                  />
                  Video
                </label>
                <label className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={wantImages}
                    onChange={(e) => setWantImages(e.target.checked)}
                    className="size-4 accent-(--color-accent)"
                  />
                  Images
                </label>
              </div>
            </div>
            {wantVideo && (
              <div className="flex flex-col gap-1.5 text-sm">
                <span className="text-ink-dim">Video tracks</span>
                <div className="flex flex-wrap gap-x-6 gap-y-2">
                  {VIDEO_MODES.map(([mode, label]) => (
                    <label key={mode} className="flex items-center gap-2">
                      <input
                        type="radio"
                        name="videoMode"
                        checked={videoMode === mode}
                        onChange={() => setVideoMode(mode)}
                        className="size-4 accent-(--color-accent)"
                      />
                      {label}
                    </label>
                  ))}
                </div>
                {videoMode === "audioOnly" && (
                  <div className="mt-1 flex flex-col gap-1.5">
                    <span className="text-ink-dim">Audio format</span>
                    <div className="flex flex-wrap gap-x-6 gap-y-2">
                      {AUDIO_FORMATS.map(([fmt, label]) => (
                        <label key={fmt} className="flex items-center gap-2">
                          <input
                            type="radio"
                            name="audioFormat"
                            checked={audioFormat === fmt}
                            onChange={() => setAudioFormat(fmt)}
                            className="size-4 accent-(--color-accent)"
                          />
                          {label}
                        </label>
                      ))}
                    </div>
                    {audioFormat === "opus" && (
                      <span className="text-xs leading-5 text-ink-dim">
                        Original quality, but many editors (DaVinci, Premiere)
                        won't open it. For editing, use MP3 or WAV.
                      </span>
                    )}
                  </div>
                )}
                {videoMode !== "audioOnly" && (
                  <div className="mt-1 flex flex-col gap-1.5">
                    <span className="text-ink-dim">Max resolution</span>
                    <div className="flex flex-wrap gap-x-6 gap-y-2">
                      {RESOLUTIONS.map(([h, label]) => (
                        <label key={h} className="flex items-center gap-2">
                          <input
                            type="radio"
                            name="maxHeight"
                            checked={maxHeight === h}
                            onChange={() => setMaxHeight(h)}
                            className="size-4 accent-(--color-accent)"
                          />
                          {label}
                        </label>
                      ))}
                    </div>
                  </div>
                )}
                {videoMode !== "audioOnly" && (
                  <div className="mt-1 flex flex-col gap-1.5">
                    <span className="text-ink-dim">Video format</span>
                    <div className="flex flex-wrap gap-x-6 gap-y-2">
                      {VIDEO_FORMATS.map(([fmt, label]) => (
                        <label key={fmt} className="flex items-center gap-2">
                          <input
                            type="radio"
                            name="videoFormat"
                            checked={videoFormat === fmt}
                            onChange={() => setVideoFormat(fmt)}
                            className="size-4 accent-(--color-accent)"
                          />
                          {label}
                        </label>
                      ))}
                    </div>
                    {videoFormat === "mp4" && (
                      <span className="text-xs leading-5 text-ink-dim">
                        Same quality, just an MP4 container: plays on media
                        players and TVs. Some videos (VP9/AV1) can still give
                        editors trouble: that's what the editing option is for.
                      </span>
                    )}
                    {videoFormat === "editing" && (
                      <span className="text-xs leading-5 text-ink-dim">
                        Picks the H.264 track: guaranteed to open in
                        DaVinci/Premiere, with no re-encoding. On many sites
                        H.264 only goes up to 1080p.
                      </span>
                    )}
                  </div>
                )}
                {videoMode !== "audioOnly" && (
                  <div className="mt-1 flex flex-col gap-1.5">
                    <span className="text-ink-dim">Subtitles</span>
                    <div className="flex flex-wrap gap-x-6 gap-y-2">
                      {SUBS_MODES.map(([mode, label]) => (
                        <label key={mode} className="flex items-center gap-2">
                          <input
                            type="radio"
                            name="subs"
                            checked={subs === mode}
                            onChange={() => setSubs(mode)}
                            className="size-4 accent-(--color-accent)"
                          />
                          {label}
                        </label>
                      ))}
                    </div>
                    {subs !== "no" && (
                      <span className="text-xs leading-5 text-ink-dim">
                        In English, if available (auto-generated too).
                        {subs === "embed" &&
                          " With the Original container some videos (.webm) won't take them: in that case the .srt file is saved alongside anyway."}
                      </span>
                    )}
                  </div>
                )}
              </div>
            )}
            <div className="flex flex-col gap-1.5 text-sm">
              <span className="text-ink-dim">Use browser login</span>
              <select
                value={cookies}
                onChange={(e) => setCookies(e.target.value as CookiesBrowser)}
                className={inputCls}
              >
                {COOKIE_BROWSERS.map(([value, label]) => (
                  <option key={value} value={value}>
                    {label}
                  </option>
                ))}
              </select>
              {cookies !== "" && (
                <span className="text-xs leading-5 text-ink-dim">
                  For images from sites that require signing in: reuses the
                  session already open in Firefox (you must be logged into the
                  site there, with Firefox closed during the download). Doesn't
                  affect videos.
                </span>
              )}
            </div>
            <div className="flex flex-col gap-1.5 text-sm">
              <span className="text-ink-dim">Destination folder</span>
              <div className="flex gap-2">
                <input
                  value={outputDir}
                  onChange={(e) => setOutputDir(e.target.value)}
                  spellCheck={false}
                  placeholder={
                    isRemote
                      ? "Path on the PC (e.g. C:\\Downloads)"
                      : "No folder selected"
                  }
                  className={inputCls + " font-mono text-xs"}
                />
                <button
                  onClick={isRemote ? openFolderPicker : onBrowse}
                  className="shrink-0 rounded-md border border-line bg-panel-2 px-4 py-2 text-sm font-medium
                             hover:border-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  Browse
                </button>
              </div>
              {isRemote && (
                <span className="text-xs text-ink-dim">
                  Files are saved on the PC, in this folder.
                </span>
              )}
            </div>
          </section>

          <div className="flex gap-2">
            <button
              onClick={onStart}
              disabled={!canStart}
              title="Start the download (Ctrl+Enter)"
              className="flex-1 rounded-lg bg-accent py-3 text-base font-bold text-accent-ink transition-colors
                         hover:bg-accent-strong disabled:cursor-not-allowed disabled:opacity-40
                         focus:outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
            >
              {busy
                ? "Running..."
                : `Start Download${links.length > 1 ? ` (${links.length})` : ""}`}
            </button>
            {busy && (
              <button
                onClick={() => cancelDownload()}
                className="rounded-lg border border-line bg-panel px-5 py-3 text-base font-semibold
                           hover:border-err hover:text-err
                           focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
              >
                Cancel
              </button>
            )}
          </div>
        </div>

        {/* Pannello destro: attività — schede uniformi, una per elemento */}
        <div className="flex min-h-full flex-col gap-5">
          {/* Coda rimasta a metà nella sessione precedente: si riprende solo
              su scelta esplicita (niente download a sorpresa all'avvio) */}
          {interrupted && !busy && (
            <section className="flex flex-wrap items-center gap-3 rounded-lg border border-accent/50 bg-accent/10 p-4">
              <span className="flex-1 text-sm">
                {interrupted.tasks.length === 1
                  ? "1 unfinished download"
                  : `${interrupted.tasks.length} unfinished downloads`}{" "}
                from the last session.
              </span>
              <div className="flex shrink-0 gap-2">
                <button
                  onClick={onResume}
                  className="rounded-md bg-accent px-4 py-2 text-sm font-bold text-accent-ink
                             hover:bg-accent-strong focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  Resume
                </button>
                <button
                  onClick={onDiscard}
                  className="rounded-md border border-line bg-panel px-4 py-2 text-sm font-medium
                             hover:border-ink-dim focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  Discard
                </button>
              </div>
            </section>
          )}

          {timeline.length > 0 ? (
            <section className="rounded-lg border border-line bg-panel p-4">
              <h2 className={sectionTitleCls + " mb-3"}>
                Activity{" "}
                <span className="text-ink-faint">· {timeline.length}</span>
              </h2>
              <div className="flex max-h-[32rem] flex-col gap-2.5 overflow-y-auto pr-1">
                {timeline.map((t, i) => (
                  <TaskCard
                    key={`${i}-${t.url}`}
                    t={t}
                    canAct={!busy}
                    onRetry={() => onRetry(t)}
                    onRemove={() => onRemove(i)}
                  />
                ))}
              </div>
            </section>
          ) : (
            !busy &&
            !interrupted && (
              <section className="flex min-h-64 flex-1 flex-col items-center justify-center gap-3 rounded-lg border border-dashed border-line bg-panel/50 p-8 text-center">
                <span
                  aria-hidden
                  className="flex size-12 items-center justify-center rounded-full bg-panel-2 text-xl text-ink-dim"
                >
                  ↓
                </span>
                <p className="text-sm font-medium">Nothing yet</p>
                <p className="max-w-xs text-xs leading-5 text-ink-dim">
                  Set your links and options, then hit Start Download: preview,
                  progress and speed will show up here in real time.
                </p>
              </section>
            )
          )}

          {/* Pannello errori: compare a coda ferma solo se qualcosa è fallito.
              Mostra il link che HAI incollato (è tuo, non un dato da nascondere)
              e il motivo tradotto, senza esporre i log grezzi dei motori. */}
          {!busy && failedItems.length > 0 && (
            <section className="rounded-lg border border-err/40 bg-err/5 p-4">
              <div className="mb-3 flex items-center gap-2">
                <span className="flex size-5 items-center justify-center rounded-full bg-err/15 text-xs font-bold text-err">
                  !
                </span>
                <h2 className={sectionTitleCls}>
                  {failedItems.length === 1
                    ? "1 failed download"
                    : `${failedItems.length} failed downloads`}
                </h2>
              </div>
              <ul className="flex flex-col gap-3">
                {failedItems.map((t, i) => (
                  <li key={i} className="flex flex-col gap-0.5">
                    <span className="truncate font-mono text-xs text-ink">
                      {t.url}
                    </span>
                    <span className="text-xs leading-5 text-err/90">
                      {t.reason ?? "Couldn't download this link"}
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          )}

          {status && (
            <output className="rounded-md border border-line bg-panel p-3 font-mono text-xs leading-5 text-ink-dim">
              {status}
            </output>
          )}

        </div>
        </main>
      ) : view === "history" ? (
        /* Vista Cronologia: lo storico persistente dei download conclusi */
        <main className="mt-6 flex flex-1 justify-center">
          <section className="w-full max-w-3xl rounded-lg border border-line bg-panel p-4">
            <div className="mb-3 flex items-center justify-between gap-3">
              <h2 className={sectionTitleCls}>
                History{" "}
                {history && history.length > 0 && (
                  <span className="text-ink-faint">· {history.length}</span>
                )}
              </h2>
              {history && history.length > 0 && (
                <button
                  onClick={onClearHistory}
                  className="rounded-md border border-line bg-panel-2 px-3 py-1.5 text-xs font-medium
                             text-ink-dim hover:border-err hover:text-err
                             focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  Clear
                </button>
              )}
            </div>
            {history == null ? (
              <p className="py-10 text-center text-sm text-ink-dim">
                Loading history…
              </p>
            ) : history.length === 0 ? (
              <div className="flex flex-col items-center gap-3 py-12 text-center">
                <span
                  aria-hidden
                  className="flex size-12 items-center justify-center rounded-full bg-panel-2 text-xl text-ink-dim"
                >
                  ✓
                </span>
                <p className="text-sm font-medium">No downloads yet</p>
                <p className="max-w-xs text-xs leading-5 text-ink-dim">
                  Here you'll find every finished download, including past
                  sessions, with shortcuts to find the files again.
                </p>
              </div>
            ) : (
              <div className="flex max-h-[36rem] flex-col gap-2.5 overflow-y-auto pr-1">
                {history.map((e, i) => (
                  <HistoryRow
                    key={`${e.when}-${i}`}
                    e={e}
                    busy={busy}
                    onRedownload={() => onRedownload(e)}
                  />
                ))}
              </div>
            )}
          </section>
        </main>
      ) : view === "settings" ? (
        /* Vista Impostazioni: preferenze dell'app, non del singolo download */
        <main className="mt-6 flex flex-1 justify-center">
          <div className="flex w-full max-w-xl flex-col gap-5">
            <section className="flex flex-col gap-4 rounded-lg border border-line bg-panel p-4">
              <h2 className={sectionTitleCls}>General</h2>
              <label className="flex items-center justify-between gap-3 text-sm">
                <span>
                  Start with Windows
                  <span className="mt-0.5 block text-xs text-ink-dim">
                    Starts hidden in the tray, server ready for your phone
                  </span>
                </span>
                <input
                  type="checkbox"
                  checked={autostart}
                  onChange={(e) => toggleAutostart(e.target.checked)}
                  className="size-5 shrink-0 accent-(--color-accent)"
                />
              </label>
              <label className="flex items-center justify-between gap-3 text-sm">
                <span>
                  Notify when the queue ends
                  <span className="mt-0.5 block text-xs text-ink-dim">
                    Windows notification when downloads finish (handy with the
                    app in the tray)
                  </span>
                </span>
                <input
                  type="checkbox"
                  checked={notifyFinish}
                  onChange={(e) => setNotifyFinish(e.target.checked)}
                  className="size-5 shrink-0 accent-(--color-accent)"
                />
              </label>
              <p className="text-xs leading-5 text-ink-dim">
                Closing the window with ✕ keeps the app running in the tray
                (next to the clock). To quit for real: right-click the tray icon
                → Quit.
              </p>
            </section>

            <section className="flex flex-col gap-4 rounded-lg border border-line bg-panel p-4">
              <h2 className={sectionTitleCls}>Download</h2>
              <label className="flex items-center justify-between gap-3 text-sm">
                <span>
                  Tags, cover art and chapters in files
                  <span className="mt-0.5 block text-xs text-ink-dim">
                    Title and artist in MP3s with cover art, metadata and
                    chapters in videos. No quality loss. Cover art in videos
                    requires the MP4 format.
                  </span>
                </span>
                <input
                  type="checkbox"
                  checked={enrich}
                  onChange={(e) => setEnrich(e.target.checked)}
                  className="size-5 shrink-0 accent-(--color-accent)"
                />
              </label>
              <div className="flex flex-col gap-1.5 text-sm">
                <span className="text-ink-dim">Video speed</span>
                <div className="flex flex-wrap gap-x-6 gap-y-2">
                  {SPEED_LEVELS.map(([n, label]) => (
                    <label key={n} className="flex items-center gap-2">
                      <input
                        type="radio"
                        name="concurrency"
                        checked={concurrency === n}
                        onChange={() => setConcurrency(n)}
                        className="size-4 accent-(--color-accent)"
                      />
                      {label}
                    </label>
                  ))}
                </div>
                <span className="text-xs leading-5 text-ink-dim">
                  Downloads several chunks of the video at once. On fiber,
                  "Fast" or "Turbo" fills the line better; on slow connections
                  the difference is minimal. Doesn't apply to "Audio only".
                </span>
              </div>
            </section>

            <section className="flex flex-col gap-4 rounded-lg border border-line bg-panel p-4">
              <h2 className={sectionTitleCls}>Updates</h2>
              <div className="flex items-center justify-between gap-3">
                <span className="text-sm">
                  {appVersion ? `Harvest ${appVersion}` : "Harvest"}
                  <span className="mt-0.5 block text-xs text-ink-dim">
                    {updateLabel(update)}
                  </span>
                </span>
                {update.status === "available" ||
                update.status === "downloading" ||
                update.status === "ready" ? null : (
                  <button
                    onClick={() => checkUpdate(setUpdate)}
                    disabled={update.status === "checking"}
                    className="shrink-0 rounded-md border border-line bg-panel-2 px-4 py-2 text-sm font-medium
                               hover:border-accent disabled:opacity-50
                               focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                  >
                    {update.status === "checking" ? "Checking..." : "Check"}
                  </button>
                )}
              </div>

              {/* Motore video: qui è sempre visibile (la pillola nell'header
                  compare solo quando c'è un aggiornamento), così si può
                  controllare a mano e vedere la versione in uso. */}
              <div className="flex items-center justify-between gap-3 border-t border-line pt-4">
                <span className="text-sm">
                  Video engine
                  <span className="mt-0.5 block font-mono text-xs text-ink-dim">
                    {engine?.current
                      ? `${engine.name} ${engine.current}`
                      : "Version not available"}
                    {engine?.updateAvailable && ` → ${engine.latest}`}
                  </span>
                  {engineMsg && (
                    <span className="mt-0.5 block text-xs text-ink-dim">
                      {engineMsg}
                    </span>
                  )}
                </span>
                <button
                  onClick={
                    engine?.updateAvailable
                      ? doUpdateEngine
                      : () => {
                          setEngineMsg("");
                          checkEngine()
                            .then((e) => {
                              setEngine(e);
                              if (!e.updateAvailable)
                                setEngineMsg("The engine is already up to date");
                            })
                            .catch(() => setEngineMsg("Check failed"));
                        }
                  }
                  disabled={engineBusy}
                  className="shrink-0 rounded-md border border-line bg-panel-2 px-4 py-2 text-sm font-medium
                             hover:border-accent disabled:opacity-50
                             focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  {engineBusy
                    ? "Updating..."
                    : engine?.updateAvailable
                      ? "Update"
                      : "Check"}
                </button>
              </div>
            </section>
          </div>
        </main>
      ) : (
        /* Vista Remote: tutto quello che serve per collegare il telefono */
        <main className="mt-6 flex flex-1 items-start justify-center">
          <section className="flex w-full max-w-md flex-col items-center gap-6 rounded-lg border border-line bg-panel p-8 text-center">
            <h2 className={sectionTitleCls}>Phone access</h2>
            {srv && srv.port == null ? (
              <p className="max-w-xs text-sm leading-6 text-err">
                The server isn't running: no port available. Close any other
                instances of the app and restart it.
              </p>
            ) : srv ? (
              <>
                <div className="rounded-lg bg-white p-3">
                  <QRCode
                    value={srv.addresses[srv.addresses.length - 1] ?? ""}
                    size={200}
                  />
                </div>
                <div className="flex w-full flex-col gap-1.5">
                  {srv.addresses.map((a) => (
                    <span
                      key={a}
                      className="select-all truncate font-mono text-sm"
                    >
                      {a}
                    </span>
                  ))}
                </div>
                <div className="flex flex-col gap-1">
                  <span className="text-xs uppercase tracking-widest text-ink-dim">
                    PIN
                  </span>
                  <span className="font-mono text-2xl font-semibold tracking-[0.3em] text-accent">
                    {srv.pin}
                  </span>
                </div>
                <p className="max-w-xs text-xs leading-5 text-ink-dim">
                  Scan the QR code or open one of the addresses in your phone's
                  browser, on the same Wi-Fi network as the PC. The PIN is only
                  asked the first time. With the window closed (✕) the app stays
                  in the tray and your phone keeps working.
                </p>
              </>
            ) : (
              <p className="text-sm text-ink-dim">
                Server unavailable: restart the app.
              </p>
            )}
          </section>
        </main>
      )}

      {browser && (
        <Modal onClose={closeBrowser} label="Choose the folder on the PC">
          <>
            <div className="border-b border-line p-4">
              <h2 className={sectionTitleCls}>Choose the folder on the PC</h2>
              <p className="mt-2 truncate font-mono text-xs text-ink-dim">
                {browser.path ?? "Pick a starting point"}
              </p>
            </div>

            {/* Scorciatoie */}
            <div className="flex flex-wrap gap-2 border-b border-line p-4">
              {browser.shortcuts.map((s) => (
                <button
                  key={s.path}
                  onClick={() => browseTo(s.path)}
                  className="rounded-full border border-line bg-panel-2 px-3 py-1 text-xs font-medium
                             hover:border-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  {s.name}
                </button>
              ))}
            </div>

            {/* Nuova cartella (solo dentro un percorso valido) */}
            {browser.path &&
              (newFolder == null ? (
                <button
                  onClick={() => setNewFolder("")}
                  className="mx-4 mt-3 flex items-center gap-2 self-start rounded-md border border-line bg-panel-2 px-3 py-1.5 text-xs font-medium
                             hover:border-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  <span aria-hidden>+</span> New folder
                </button>
              ) : (
                <form
                  onSubmit={(e) => {
                    e.preventDefault();
                    confirmNewFolder();
                  }}
                  className="mx-4 mt-3 flex gap-2"
                >
                  <input
                    value={newFolder}
                    onChange={(e) => setNewFolder(e.target.value)}
                    autoFocus
                    placeholder="Folder name"
                    className={inputCls + " text-sm"}
                  />
                  <button
                    type="submit"
                    disabled={!newFolder.trim() || browserBusy}
                    className="shrink-0 rounded-md bg-accent px-3 py-2 text-sm font-bold text-accent-ink
                               hover:bg-accent-strong disabled:opacity-40
                               focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                  >
                    Create
                  </button>
                  <button
                    type="button"
                    onClick={() => setNewFolder(null)}
                    className="shrink-0 rounded-md border border-line px-3 py-2 text-sm
                               hover:border-ink-dim focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                  >
                    ✕
                  </button>
                </form>
              ))}

            {/* Lista sottocartelle */}
            <div className="min-h-0 flex-1 overflow-y-auto p-2">
              {browser.parent != null && (
                <button
                  onClick={() => browseTo(browser.parent)}
                  className="flex w-full items-center gap-2 rounded-md px-3 py-2 text-left text-sm text-ink-dim
                             hover:bg-panel-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  <span aria-hidden>↑</span> Parent folder
                </button>
              )}
              {browser.entries.map((e) => (
                <button
                  key={e.path}
                  onClick={() => browseTo(e.path)}
                  className="flex w-full items-center gap-2 truncate rounded-md px-3 py-2 text-left text-sm
                             hover:bg-panel-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  <span aria-hidden className="text-ink-dim">
                    ▸
                  </span>
                  <span className="truncate">{e.name}</span>
                </button>
              ))}
              {browser.path && browser.entries.length === 0 && (
                <p className="px-3 py-6 text-center text-xs text-ink-dim">
                  No subfolders here.
                </p>
              )}
            </div>

            {/* Azioni */}
            <div className="flex gap-2 border-t border-line p-4">
              <button
                onClick={closeBrowser}
                className="rounded-md border border-line bg-panel px-4 py-2 text-sm font-medium
                           hover:border-ink-dim focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
              >
                Cancel
              </button>
              <button
                disabled={!browser.path || browserBusy}
                onClick={() => {
                  if (browser.path) setOutputDir(browser.path);
                  closeBrowser();
                }}
                className="flex-1 rounded-md bg-accent px-4 py-2 text-sm font-bold text-accent-ink
                           hover:bg-accent-strong disabled:cursor-not-allowed disabled:opacity-40
                           focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
              >
                Save here
              </button>
            </div>
          </>
        </Modal>
      )}
    </div>
  );
}
