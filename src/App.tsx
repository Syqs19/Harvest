import { useEffect, useMemo, useState, type ReactNode } from "react";
import QRCode from "react-qr-code";
import {
  startDownload,
  cancelDownload,
  onDownloadEvent,
  pickOutputFolder,
  fetchServerState,
  fetchServerInfo,
  getAutostart,
  setAutostart,
  runUpdate,
  type UpdateState,
  browseDir,
  createDir,
  isRemote,
  getPin,
  savePin,
  PinError,
  type Engine,
  type VideoMode,
  type Outcome,
  type ServerInfo,
  type DirListing,
  type CookiesBrowser,
} from "./api";
import { findNumbers, generateSeriesUrls } from "./urls";

interface TimelineItem {
  url: string;
  engine: Engine;
  status: "pending" | "running" | Outcome;
}

function updateLabel(u: UpdateState): string {
  switch (u.status) {
    case "idle":
      return "Verifica se c'è una nuova versione";
    case "checking":
      return "Controllo in corso...";
    case "none":
      return "Sei già all'ultima versione";
    case "available":
      return `Nuova versione ${u.version} trovata, scarico...`;
    case "downloading":
      return `Scarico l'aggiornamento... ${u.percent.toFixed(0)}%`;
    case "ready":
      return "Aggiornamento pronto: riavvio...";
    case "error":
      return `Errore: ${u.message}`;
  }
}

const VIDEO_MODES: [VideoMode, string][] = [
  ["full", "Video + audio"],
  ["videoOnly", "Solo video"],
  ["audioOnly", "Solo audio"],
];

// Solo Firefox: i browser Chromium (Chrome/Edge/Brave) da Chrome 127 blindano
// i cookie e sono illeggibili dai programmi esterni, quindi non li offriamo.
const COOKIE_BROWSERS: [CookiesBrowser, string][] = [
  ["", "Nessuno"],
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

  // Modalità server: gestione PIN e dati per il pannello "Accesso dal telefono"
  const [pinNeeded, setPinNeeded] = useState(false);
  const [pinInput, setPinInput] = useState(getPin());
  const [authTick, setAuthTick] = useState(0);
  const [srv, setSrv] = useState<ServerInfo | null>(null);
  // Vista corrente (solo desktop): Download o Remote
  const [view, setView] = useState<"download" | "remote">("download");
  // Selettore cartelle: naviga il disco del PC dal telefono
  const [browser, setBrowser] = useState<DirListing | null>(null);
  const [browserBusy, setBrowserBusy] = useState(false);
  const [newFolder, setNewFolder] = useState<string | null>(null); // null = form chiuso

  // Avanzamento del link in corso, alimentato dagli eventi del backend
  const [prog, setProg] = useState<{
    index: number;
    total: number;
    url: string;
    engine: Engine;
    percent: number | null;
    line: string;
  } | null>(null);

  function handleEvent(ev: Parameters<Parameters<typeof onDownloadEvent>[0]>[0]) {
    switch (ev.kind) {
      case "queueStart":
        setTimeline(
          ev.tasks.map((t) => ({ ...t, status: "pending" as const })),
        );
        setBusy(true);
        break;
      case "itemStart":
        setProg({
          index: ev.index,
          total: ev.total,
          url: ev.url,
          engine: ev.engine,
          percent: null,
          line: "",
        });
        setTimeline((tl) =>
          tl.map((t, i) => (i === ev.index ? { ...t, status: "running" } : t)),
        );
        break;
      case "progress":
        setProg((p) => p && { ...p, percent: ev.percent });
        break;
      case "line":
        setProg((p) => p && { ...p, line: ev.line });
        break;
      case "itemDone":
        setTimeline((tl) =>
          tl.map((t, i) => (i === ev.index ? { ...t, status: ev.outcome } : t)),
        );
        break;
      case "finished": {
        setBusy(false);
        setProg(null);
        const parts = [`${ev.ok} completati`];
        if (ev.failed > 0) parts.push(`${ev.failed} falliti`);
        if (ev.nothing > 0)
          parts.push(`${ev.nothing} senza contenuto per il motore scelto`);
        setStatus((ev.cancelled ? "Annullato — " : "Finito: ") + parts.join(", "));
        // I task mai partiti restano "in attesa": dopo un annullo li segno come tali
        if (ev.cancelled)
          setTimeline((tl) =>
            tl.map((t) =>
              t.status === "pending" || t.status === "running"
                ? { ...t, status: "failed" }
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

    async function init() {
      if (isRemote) {
        // Dal telefono: prima la fotografia della coda (e la verifica del PIN)...
        try {
          const s = await fetchServerState();
          if (dead) return;
          setPinNeeded(false);
          setBusy(s.busy);
          setTimeline(
            s.timeline.map((t) => ({
              url: t.url,
              engine: t.engine,
              status: t.status as TimelineItem["status"],
            })),
          );
          setOutputDir((d) => d || s.lastOutputDir);
        } catch (e) {
          if (!dead && e instanceof PinError) setPinNeeded(true);
          else if (!dead) setStatus(`Connessione al PC fallita: ${e}`);
          return;
        }
      }
      // ...poi gli eventi in tempo reale (nell'app: da subito)
      const un = await onDownloadEvent(handleEvent);
      if (dead) un();
      else unlisten = un;
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
  useEffect(() => {
    if (!isRemote) {
      fetchServerInfo().then(setSrv).catch(() => {});
      getAutostart().then(setAutostartState).catch(() => {});
    }
  }, []);

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

  const links = bulk
    ? seriesUrls
    : singleUrls.split("\n").map((l) => l.trim()).filter(Boolean);

  const canStart =
    links.length > 0 && outputDir !== "" && (wantVideo || wantImages) && !busy;

  async function onBrowse() {
    const folder = await pickOutputFolder();
    if (folder) setOutputDir(folder);
  }

  async function openFolderPicker() {
    setBrowserBusy(true);
    try {
      setBrowser(await browseDir(outputDir || null));
    } catch (e) {
      setStatus(`Impossibile leggere le cartelle: ${e}`);
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
        cookies,
        outputDir,
      );
      // Da qui in poi lo stato arriva dagli eventi del backend (fino a "finished")
    } catch (err) {
      setBusy(false);
      setStatus(`Errore: ${err}`);
    }
  }

  function statusIcon(s: TimelineItem["status"]) {
    switch (s) {
      case "pending":
        return (
          <span className="mt-1 size-4 shrink-0 rounded-full border-2 border-line" />
        );
      case "running":
        return (
          <span className="relative mt-1 flex size-4 shrink-0 items-center justify-center">
            <span className="absolute size-4 animate-ping rounded-full bg-accent/30" />
            <span className="size-2.5 rounded-full bg-accent" />
          </span>
        );
      case "ok":
        return (
          <span className="mt-1 flex size-4 shrink-0 items-center justify-center rounded-full bg-ok/15 text-[10px] font-bold text-ok">
            ✓
          </span>
        );
      case "failed":
        return (
          <span className="mt-1 flex size-4 shrink-0 items-center justify-center rounded-full bg-err/15 text-[10px] font-bold text-err">
            ✕
          </span>
        );
      case "nothing":
        return (
          <span className="mt-1 flex size-4 shrink-0 items-center justify-center rounded-full bg-panel-2 text-[10px] font-bold text-ink-dim">
            –
          </span>
        );
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
            Inserisci il PIN mostrato nell'app sul PC, nel pannello "Accesso
            dal telefono".
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
            Entra
          </button>
        </form>
      </div>
    );
  }

  return (
    <div className="mx-auto flex min-h-screen w-full max-w-6xl flex-col px-4 py-6 sm:px-8">
      <header className="flex items-center justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">
            Harvest<span className="text-accent">.</span>
          </h1>
          <p className="text-sm text-ink-dim">
            Download locale di video e immagini
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-3">
          {!isRemote && (
            <nav className="flex gap-1 rounded-lg bg-panel p-1">
              {(
                [
                  ["download", "Download"],
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
          <div className="flex items-center gap-2 rounded-full border border-line bg-panel px-3 py-1.5 text-xs font-medium text-ink-dim">
            <span
              className={
                "size-2 rounded-full " +
                (busy ? "animate-pulse bg-accent" : "bg-ok")
              }
            />
            {busy ? "In corso" : "Pronto"}
          </div>
        </div>
      </header>

      {view === "download" ? (
        <main className="mt-6 grid flex-1 items-start gap-5 lg:grid-cols-[minmax(0,26rem)_minmax(0,1fr)]">
        {/* Pannello sinistro: sorgente, opzioni, azioni */}
        <div className="flex flex-col gap-5">
          <section className="flex flex-col gap-4 rounded-lg border border-line bg-panel p-4">
            <div className="flex items-center justify-between">
              <h2 className={sectionTitleCls}>Sorgente</h2>
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
                <span className="text-ink-dim">Link (uno per riga)</span>
                <textarea
                  value={singleUrls}
                  onChange={(e) => setSingleUrls(e.target.value)}
                  rows={5}
                  spellCheck={false}
                  placeholder={"https://esempio.com/video\nhttps://esempio.com/galleria"}
                  className={inputCls + " resize-y font-mono text-xs leading-5"}
                />
              </label>
            ) : (
              <>
                <label className="flex flex-col gap-1.5 text-sm">
                  <span className="text-ink-dim">
                    Incolla un link di esempio (es. la prima pagina della serie)
                  </span>
                  <input
                    value={exampleUrl}
                    onChange={(e) => {
                      setExampleUrl(e.target.value);
                      setSelIdx(null);
                    }}
                    spellCheck={false}
                    placeholder="https://esempio.com/galleria/pagina-1"
                    className={inputCls + " font-mono text-xs"}
                  />
                </label>

                {exampleUrl.trim() !== "" && numbers.length === 0 && (
                  <p className="text-xs text-ink-dim">
                    Il link non contiene numeri: non posso generare una serie.
                  </p>
                )}

                {numbers.length > 0 && (
                  <div className="flex flex-col gap-1.5 text-sm">
                    <span className="text-ink-dim">
                      {numbers.length === 1
                        ? "Numero che varierà (rilevato automaticamente)"
                        : "Clicca sul numero che deve variare"}
                    </span>
                    <p className="break-all rounded-md border border-line bg-panel-2 px-3 py-2 font-mono text-xs leading-6">
                      {renderSegmentedUrl()}
                    </p>
                  </div>
                )}

                {selected && (
                  <>
                    <div className="flex flex-wrap items-end gap-4">
                      <label className="flex flex-col gap-1.5 text-sm">
                        <span className="text-ink-dim">Da</span>
                        <input
                          type="number"
                          value={from}
                          onChange={(e) => setFrom(e.target.valueAsNumber)}
                          className={inputCls + " w-24"}
                        />
                      </label>
                      <label className="flex flex-col gap-1.5 text-sm">
                        <span className="text-ink-dim">A</span>
                        <input
                          type="number"
                          value={to}
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
                        Zeri iniziali (es. 01)
                      </label>
                    </div>
                    {seriesUrls.length > 0 && (
                      <p className="break-all font-mono text-xs text-ink-dim">
                        {seriesUrls.length} link: {seriesUrls[0]}
                        {seriesUrls.length > 1 &&
                          ` ... ${seriesUrls[seriesUrls.length - 1]}`}
                      </p>
                    )}
                  </>
                )}
              </>
            )}
          </section>

          <section className="flex flex-col gap-4 rounded-lg border border-line bg-panel p-4">
            <h2 className={sectionTitleCls}>Opzioni</h2>
            <div className="flex flex-col gap-1.5 text-sm">
              <span className="text-ink-dim">Cosa scaricare</span>
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
                  Immagini
                </label>
              </div>
            </div>
            {wantVideo && (
              <div className="flex flex-col gap-1.5 text-sm">
                <span className="text-ink-dim">Tracce del video</span>
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
              </div>
            )}
            <div className="flex flex-col gap-1.5 text-sm">
              <span className="text-ink-dim">Usa login da browser</span>
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
                  Per le immagini da siti che richiedono l'accesso: riusa la
                  sessione già aperta in Firefox (devi essere loggato al sito
                  lì, con Firefox chiuso durante il download). Non influisce sui
                  video.
                </span>
              )}
            </div>
            <div className="flex flex-col gap-1.5 text-sm">
              <span className="text-ink-dim">Cartella di destinazione</span>
              <div className="flex gap-2">
                <input
                  value={outputDir}
                  onChange={(e) => setOutputDir(e.target.value)}
                  spellCheck={false}
                  placeholder={
                    isRemote
                      ? "Percorso sul PC (es. C:\\Download)"
                      : "Nessuna cartella selezionata"
                  }
                  className={inputCls + " font-mono text-xs"}
                />
                <button
                  onClick={isRemote ? openFolderPicker : onBrowse}
                  className="shrink-0 rounded-md border border-line bg-panel-2 px-4 py-2 text-sm font-medium
                             hover:border-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                >
                  Sfoglia
                </button>
              </div>
              {isRemote && (
                <span className="text-xs text-ink-dim">
                  I file vengono salvati sul PC, in questa cartella.
                </span>
              )}
            </div>
          </section>

          <div className="flex gap-2">
            <button
              onClick={onStart}
              disabled={!canStart}
              className="flex-1 rounded-lg bg-accent py-3 text-base font-bold text-accent-ink transition-colors
                         hover:bg-accent-strong disabled:cursor-not-allowed disabled:opacity-40
                         focus:outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
            >
              {busy
                ? "In corso..."
                : `Avvia Download${links.length > 1 ? ` (${links.length})` : ""}`}
            </button>
            {busy && (
              <button
                onClick={() => cancelDownload()}
                className="rounded-lg border border-line bg-panel px-5 py-3 text-base font-semibold
                           hover:border-err hover:text-err
                           focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
              >
                Annulla
              </button>
            )}
          </div>
        </div>

        {/* Pannello destro: attività */}
        <div className="flex min-h-full flex-col gap-5">
          {prog && (
            <section className="flex flex-col gap-3 rounded-lg border border-accent/40 bg-panel p-4">
              <div className="flex items-center justify-between">
                <span className="flex items-center gap-2 text-sm font-medium">
                  <span className="rounded bg-panel-2 px-2 py-0.5 text-xs font-semibold uppercase tracking-wide text-accent">
                    {prog.engine === "video" ? "Video" : "Immagini"}
                  </span>
                  Link {prog.index + 1} di {prog.total}
                </span>
                <span className="font-mono text-lg font-semibold text-accent">
                  {prog.percent != null ? `${prog.percent.toFixed(0)}%` : "..."}
                </span>
              </div>
              <div
                role="progressbar"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={
                  prog.percent != null ? Math.round(prog.percent) : undefined
                }
                className="h-2.5 overflow-hidden rounded-full bg-panel-2"
              >
                {prog.percent != null ? (
                  <div
                    className="h-full rounded-full bg-gradient-to-r from-accent to-accent-strong transition-[width] duration-300"
                    style={{ width: `${prog.percent}%` }}
                  />
                ) : (
                  <div className="bar-indeterminate h-full w-1/3 rounded-full bg-gradient-to-r from-accent to-accent-strong" />
                )}
              </div>
              <p className="truncate font-mono text-xs text-ink-dim">
                {prog.url}
              </p>
              {prog.line && (
                <p className="truncate font-mono text-xs text-ink-dim">
                  {prog.line}
                </p>
              )}
            </section>
          )}

          {timeline.length > 0 ? (
            <section className="rounded-lg border border-line bg-panel p-4">
              <h2 className={sectionTitleCls + " mb-3"}>Coda</h2>
              <ol className="flex max-h-[26rem] flex-col overflow-y-auto">
                {timeline.map((t, i) => (
                  <li
                    key={i}
                    className="relative flex items-start gap-3 pb-3 last:pb-0"
                  >
                    {i < timeline.length - 1 && (
                      <span
                        aria-hidden
                        className="absolute left-[7px] top-6 h-[calc(100%-1.25rem)] w-px bg-line"
                      />
                    )}
                    {statusIcon(t.status)}
                    <div className="flex min-w-0 flex-1 items-baseline gap-2">
                      <span className="shrink-0 rounded bg-panel-2 px-1.5 py-0.5 font-mono text-[10px] font-semibold uppercase tracking-wide text-ink-dim">
                        {t.engine === "video" ? "Video" : "Img"}
                      </span>
                      <span
                        className={
                          "truncate font-mono text-xs " +
                          (t.status === "running" ? "text-ink" : "text-ink-dim")
                        }
                      >
                        {t.url}
                      </span>
                      {t.status === "nothing" && (
                        <span className="shrink-0 text-xs text-ink-dim">
                          niente da scaricare
                        </span>
                      )}
                    </div>
                  </li>
                ))}
              </ol>
            </section>
          ) : (
            !prog && (
              <section className="flex min-h-64 flex-1 flex-col items-center justify-center gap-3 rounded-lg border border-dashed border-line bg-panel/50 p-8 text-center">
                <span
                  aria-hidden
                  className="flex size-12 items-center justify-center rounded-full bg-panel-2 text-xl text-ink-dim"
                >
                  ↓
                </span>
                <p className="text-sm font-medium">Nessuna attività</p>
                <p className="max-w-xs text-xs leading-5 text-ink-dim">
                  Prepara link e opzioni, poi premi Avvia Download: qui vedrai
                  avanzamento e coda in tempo reale.
                </p>
              </section>
            )
          )}

          {status && (
            <output className="rounded-md border border-line bg-panel p-3 font-mono text-xs leading-5 text-ink-dim">
              {status}
            </output>
          )}

        </div>
        </main>
      ) : (
        /* Vista Remote: tutto quello che serve per collegare il telefono */
        <main className="mt-6 flex flex-1 items-start justify-center">
          <section className="flex w-full max-w-md flex-col items-center gap-6 rounded-lg border border-line bg-panel p-8 text-center">
            <h2 className={sectionTitleCls}>Accesso dal telefono</h2>
            {srv && srv.port == null ? (
              <p className="max-w-xs text-sm leading-6 text-err">
                Il server non è attivo: nessuna porta disponibile. Chiudi
                eventuali altre istanze dell'app e riavviala.
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
                  Inquadra il QR o apri uno degli indirizzi dal browser del
                  telefono, sulla stessa rete Wi-Fi del PC. Il PIN viene
                  chiesto solo la prima volta.
                </p>

                <div className="mt-2 w-full border-t border-line pt-4">
                  <label className="flex items-center justify-between gap-3 text-sm">
                    <span className="text-left">
                      Avvia con Windows
                      <span className="mt-0.5 block text-xs text-ink-dim">
                        Parte nascosto nel tray, server già pronto
                      </span>
                    </span>
                    <input
                      type="checkbox"
                      checked={autostart}
                      onChange={(e) => toggleAutostart(e.target.checked)}
                      className="size-5 shrink-0 accent-(--color-accent)"
                    />
                  </label>
                  <p className="mt-3 text-left text-xs leading-5 text-ink-dim">
                    Chiudendo la finestra con la ✕ l'app resta attiva nel tray
                    (vicino all'orologio) e il telefono continua a funzionare.
                    Per chiudere davvero: clic destro sull'icona nel tray →
                    Esci.
                  </p>
                </div>

                <div className="mt-2 w-full border-t border-line pt-4 text-left">
                  <div className="flex items-center justify-between gap-3">
                    <span className="text-sm">
                      Aggiornamenti
                      <span className="mt-0.5 block text-xs text-ink-dim">
                        {updateLabel(update)}
                      </span>
                    </span>
                    {update.status === "available" ||
                    update.status === "downloading" ||
                    update.status === "ready" ? null : (
                      <button
                        onClick={() => runUpdate(setUpdate)}
                        disabled={update.status === "checking"}
                        className="shrink-0 rounded-md border border-line bg-panel-2 px-4 py-2 text-sm font-medium
                                   hover:border-accent disabled:opacity-50
                                   focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                      >
                        {update.status === "checking"
                          ? "Controllo..."
                          : "Controlla"}
                      </button>
                    )}
                  </div>
                </div>
              </>
            ) : (
              <p className="text-sm text-ink-dim">
                Server non disponibile: riavvia l'app.
              </p>
            )}
          </section>
        </main>
      )}

      {browser && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
          onClick={() => {
            setBrowser(null);
            setNewFolder(null);
          }}
        >
          <div
            className="flex max-h-[80vh] w-full max-w-md flex-col rounded-lg border border-line bg-panel"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="border-b border-line p-4">
              <h2 className={sectionTitleCls}>Scegli la cartella sul PC</h2>
              <p className="mt-2 truncate font-mono text-xs text-ink-dim">
                {browser.path ?? "Scegli un punto di partenza"}
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
                  <span aria-hidden>+</span> Nuova cartella
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
                    placeholder="Nome cartella"
                    className={inputCls + " text-sm"}
                  />
                  <button
                    type="submit"
                    disabled={!newFolder.trim() || browserBusy}
                    className="shrink-0 rounded-md bg-accent px-3 py-2 text-sm font-bold text-accent-ink
                               hover:bg-accent-strong disabled:opacity-40
                               focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
                  >
                    Crea
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
                  <span aria-hidden>↑</span> Cartella superiore
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
                  Nessuna sottocartella qui.
                </p>
              )}
            </div>

            {/* Azioni */}
            <div className="flex gap-2 border-t border-line p-4">
              <button
                onClick={() => setBrowser(null)}
                className="rounded-md border border-line bg-panel px-4 py-2 text-sm font-medium
                           hover:border-ink-dim focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
              >
                Annulla
              </button>
              <button
                disabled={!browser.path || browserBusy}
                onClick={() => {
                  if (browser.path) setOutputDir(browser.path);
                  setBrowser(null);
                }}
                className="flex-1 rounded-md bg-accent px-4 py-2 text-sm font-bold text-accent-ink
                           hover:bg-accent-strong disabled:cursor-not-allowed disabled:opacity-40
                           focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
              >
                Salva qui
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
