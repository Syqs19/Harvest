<div align="center">
  <img src="src-tauri/icons/128x128.png" width="96" alt="Harvest">
  <h1>Harvest</h1>
  <p><strong>A fast, local desktop media downloader.</strong></p>
</div>

---

Harvest is a lightweight desktop app for downloading **videos and images** from a
link, in high quality, straight to your PC. Minimal dark-mode interface, no
account, no external service: everything runs locally.

## Features

- **Video** — download at the best available resolution (video and audio tracks
  are merged automatically when needed). Pick a resolution cap, and choose the
  original container, MP4, or MP4 with an H.264 track for video editors.
- **Audio only** — extract the audio as MP3, WAV, or Opus.
- **Images** — download galleries and collections from forums, image hosts and
  media sites.
- **Subtitles, tags, cover art and chapters** — embed them in the file, or save
  subtitles as a separate `.srt`.
- **Bulk mode** — generate a series of links from a single example (handy for
  numbered pages).
- **Deduplication** — never save the same file twice, even when it appears on
  several pages or in different qualities.
- **Queue with live progress** — real-time speed, ETA and thumbnails, retry a
  failed item, cancel instantly. An interrupted queue can be resumed after a
  crash or restart.
- **History** — every download is saved, with links back to the file and folder.
- **Remote mode** — drive your downloads from your phone's browser over the same
  network, protected by a PIN.
- **Tray & start with Windows** — the app can stay running in the background.
- **Updates** — update the app, and the video engine itself, in one click. This
  matters: sites change often, and an outdated engine stops working.

## How it works

Harvest is an interface built with [Tauri](https://tauri.app) (Rust + web) that
orchestrates proven command-line tools:

- **[yt-dlp](https://github.com/yt-dlp/yt-dlp)** for video
- **[gallery-dl](https://github.com/mikf/gallery-dl)** for images
- **[ffmpeg](https://ffmpeg.org)** for merging tracks at full quality

## Install

Download the latest installer from the
[**Releases**](https://github.com/Syqs19/Harvest/releases/latest) page and run it.

> On first launch Windows may show "Windows protected your PC", because the app
> is not signed with a commercial certificate. Click
> **More info → Run anyway**.

## Build from source

You need [Node.js](https://nodejs.org) and [Rust](https://rustup.rs).

```bash
npm install
npm run tauri dev      # development
npm run tauri build    # build the installer
```

> **A note on the engines:** the `yt-dlp`, `gallery-dl` and `ffmpeg` binaries are
> not included in this repository (they are not this project's code). Download
> them from their own sites and drop them into `src-tauri/binaries/` under these
> names:
>
> - `yt-dlp-x86_64-pc-windows-msvc.exe`
> - `gallery-dl-x86_64-pc-windows-msvc.exe`
> - `ffmpeg-x86_64-pc-windows-msvc.exe`

## License and credits

Harvest is released under the [MIT](LICENSE) license.

The app bundles and uses third-party software, each under its own license:
yt-dlp (Unlicense), gallery-dl (GPLv2), ffmpeg (GPL/LGPL). All rights belong to
the respective projects.

Harvest is a technical tool: you are responsible for how you use it, and for
complying with the terms of the sites you download from and with copyright law.
