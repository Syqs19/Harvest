<div align="center">
  <img src="src-tauri/icons/128x128.png" width="96" alt="Harvest">
  <h1>Harvest</h1>
  <p><strong>Downloader desktop di media, locale e veloce.</strong></p>
</div>

---

Harvest è un'app desktop leggera per scaricare **video e immagini** da un link, in
alta qualità e direttamente sul tuo PC. Interfaccia minimale in dark mode,
nessun account, nessun servizio esterno: tutto gira in locale.

## Funzionalità

- **Video** — download alla massima risoluzione disponibile (unione automatica di
  video e audio quando servono).
- **Immagini** — scarica gallerie e raccolte da forum, image host e siti di media.
- **Modalità Bulk** — genera una serie di link a partire da uno di esempio
  (utile per pagine numerate).
- **Filtro per tipo** — scegli se scaricare solo video, solo immagini o entrambi.
- **Deduplica** — evita di salvare due volte lo stesso file, anche quando compare
  in più pagine o in versioni di qualità diverse.
- **Coda con progresso** — avanzamento in tempo reale, timeline degli elementi,
  annullamento immediato.
- **Modalità Remote** — comanda i download dal telefono, tramite il browser,
  sulla stessa rete (protetta da PIN).
- **Tray & avvio con Windows** — l'app può restare attiva in background.
- **Aggiornamenti** — controllo e installazione della nuova versione con un clic.

## Come funziona

Harvest è un'interfaccia costruita con [Tauri](https://tauri.app) (Rust + web) che
orchestra strumenti da riga di comando collaudati:

- **[yt-dlp](https://github.com/yt-dlp/yt-dlp)** per i video
- **[gallery-dl](https://github.com/mikf/gallery-dl)** per le immagini
- **[ffmpeg](https://ffmpeg.org)** per unire le tracce alla massima qualità

## Installazione

Scarica l'ultimo installer dalla pagina
[**Releases**](https://github.com/Syqs19/Harvest/releases/latest) ed eseguilo.

> Al primo avvio Windows potrebbe mostrare "Windows ha protetto il PC" perché
> l'app non è firmata con un certificato commerciale. Clicca su
> **Ulteriori informazioni → Esegui comunque**.

## Compilare dal sorgente

Servono [Node.js](https://nodejs.org) e [Rust](https://rustup.rs).

```bash
npm install
npm run tauri dev      # avvio in sviluppo
npm run tauri build    # crea l'installer
```

> **Nota sui motori:** i binari di `yt-dlp`, `gallery-dl` e `ffmpeg` non sono
> inclusi nel repository (non sono codice di questo progetto). Scaricali dai
> rispettivi siti e mettili in `src-tauri/binaries/` con questi nomi:
>
> - `yt-dlp-x86_64-pc-windows-msvc.exe`
> - `gallery-dl-x86_64-pc-windows-msvc.exe`
> - `ffmpeg-x86_64-pc-windows-msvc.exe`

## Licenze e crediti

Harvest è distribuito sotto licenza [MIT](LICENSE).

L'app include e utilizza software di terze parti, ciascuno con la propria licenza:
yt-dlp (Unlicense), gallery-dl (GPLv2), ffmpeg (GPL/LGPL). Tutti i diritti dei
rispettivi progetti.

Harvest è uno strumento tecnico: sei responsabile dell'uso che ne fai e del
rispetto dei termini dei siti da cui scarichi e delle leggi sul diritto d'autore.
