# CanLapse

**Timelapse recorder for drawing-app canvas windows** — Windows 10/11 x64.

Pick a canvas window once, and CanLapse quietly captures it on a schedule, keeps only the frames that
actually changed, and exports a 30 fps MP4 when you stop. Built for digital painters using SAI2,
Photoshop, CLIP STUDIO PAINT or Krita.

`v2.0.1` · single-file executable · no DLLs · English / 中文 UI (follows system language)

![Main window](docs/evidence/ui-main-en.png)

---

## Highlights

- **One file, no installation.** `CanLapse.exe` is 3.8 MB and statically links ffmpeg + libopenh264
  and the MSVC runtime. No DLLs, no .NET, no VC++ redistributable. Copy it anywhere and run.
- **Canvas-only capture.** Click the canvas border once; CanLapse records that window, not your desktop.
- **Differential frames.** A frame is saved only when the picture changes — an hour of painting becomes
  a few hundred frames instead of 100k screenshots.
- **Floating control bar.** Dark, rounded, snapped to the top-right corner of the monitor where the
  canvas lives; drag it left/right along the top edge. Pause / mark / stop, with live `00:12 · 3 frames`
  status.
- **Encoding you can watch (and cancel).** Stopping restores the main window instantly and opens a small
  progress dialog; it closes itself when the MP4 is done.
- **Bilingual UI**, English by default on non-Chinese systems, 中文 on Chinese systems — no language switch.
- **Zero UI framework.** Plain Win32 + GDI, hand-drawn widgets, dark theme.

| Floating bar | Encoding dialog |
|---|---|
| ![Bar](docs/evidence/floatbar-en.png) | ![Dialog](docs/evidence/enc-dialog.png) |

---

## Quick start

1. Extract `CanLapse-v2.0.1-win64.zip` to any writable folder (for example `D:\Tools\CanLapse`).
2. Run `CanLapse.exe`.
3. Choose the source for your app:
   - SAI2, Photoshop, Krita, and most other apps: click **Select Canvas Window** and
     choose the original canvas window. Canvas pan, zoom, and rotation are recorded.
   - SAI2 fixed full canvas: detach the navigator and click **Use SAI2 Navigator**.
   - CSP: use **Window → Canvas → New Window**, drag the new canvas tab out, then use
     **View → Fit Window Size** and click **Select Canvas Window** on that second view.
4. For CSP, keep the second view fitted to the complete canvas at a fixed angle.
5. Choose **Frame density** and **Change sensitivity**, then click **Start Recording**.
6. Draw in the original view. Use the floating bar to **Pause**, **Mark** or **Stop**.
7. The MP4 lands in `output\timelapse_YYYYMMDD-HHMMSS.mp4` next to the exe.

> The first captured frame defines the video resolution. If you resize the canvas mid-session, frames are
> scaled proportionally and letterboxed.

### Canvas window, CSP second view, and SAI2 navigator

**Select Canvas Window** records the selected original canvas window for SAI2,
Photoshop, Krita, and most other apps. Pan, zoom, and rotation are recorded as part
of the artwork session.

For CSP, a second fitted canvas view is recommended because it keeps the complete
canvas visible while the original view is used for drawing; pan, zoom, and rotation
can then be excluded from the timelapse. For SAI2, use **Use SAI2 Navigator** when a
fixed full-canvas recording is required.

CLI equivalents:

```powershell
canlapse-cli record --navigator
canlapse-cli record --hwnd 0x1234 --full-view
```

`cache\` (session frames) and `output\` (videos) are created automatically next to the executable, so a
USB-stick copy stays self-contained.

---

## Requirements

| Item | Requirement |
|---|---|
| OS | Windows 10 / 11, 64-bit (x64). ARM64 works through emulation. |
| Desktop composition | Recommended. Canvas-window capture uses DWM when available and falls back to direct capture; SAI2 navigator mode always uses direct capture. |
| Runtime | **None.** No DLLs, no .NET, no Visual C++ redistributable, no installer. |
| Drawing apps | Any app with a floating canvas window: SAI2, Photoshop, CLIP STUDIO PAINT, Krita… |

---

## How it works

```
canvas HWND ──► DWM thumbnail (DwmRegisterThumbnail + rcSource crop)
                     │
                     ▼
              BGRA frame ──► pixel diff (threshold by changed-pixel ratio)
                                │ changed
                                ▼
             libopenh264 (H.264 High, 30 fps, 1 s GOP)
                                │
                 ┌──────────────┴──────────────┐
                 ▼                             ▼
      stream.mp4 (fragmented)       stream.packets (packet index)
                 │
                 ▼ finalize: packet stream-copy, no video re-encode
      output\timelapse_YYYYMMDD-HHMMSS.mp4
```

- Capture uses the **DWM thumbnail** of the target window, read back through
  `PrintWindow(host, PW_RENDERFULLCONTENT)`, so it works for GPU-composited apps and never steals focus.
- Recording **auto-pauses when another app is in the foreground** (you are not painting) and resumes when
  you return to the canvas.
- Encoding is a hand-written FFI layer over avcodec/avformat; no `ffmpeg.exe`, no subprocess.
- Frames are encoded during recording. Finalization copies the existing H.264 packets into a regular MP4;
  it does not save image files and does not encode the video a second time.
- If a target is mostly or completely outside the virtual desktop, CanLapse automatically switches from
  the DWM thumbnail path to direct `PrintWindow(PW_RENDERFULLCONTENT)` capture.
- Canvas-window capture uses DWM when available and automatically falls back to direct source capture
  if the DWM path fails or the window leaves the visible desktop.

---

## Settings

| Setting | Options | Default |
|---|---|---|
| Frame density | 1 frame / 5 s · 1 frame / 2 s · 1 frame / s · 2 frames / s | **1 frame / 5 s** |
| Change sensitivity | 1 = 0.007 · 2 = 0.006 · 3 = 0.005 · 4 = 0.004 · 5 = 0.003 · **6 = 0.002** · 7 = 0.001 · 8 = 0.00075 · 9 = 0.0005 | **6** |
| Playback rate | fixed 30 fps | — |
| Quality | fixed medium (H.264, ~0.06 bpp) | — |
| Auto-clean cache | delete streaming session data after a successful finalize | on |

*Frame density* is how often the canvas is checked; *sensitivity* decides whether the change is worth a
frame. Large canvases need higher sensitivity (5–8).

---

## Files and logs

| Path | Content |
|---|---|
| `output\timelapse_YYYYMMDD-HHMMSS.mp4` | exported video, timestamped at stop time (no overwrites) |
| `cache\<timestamp>_<process>\` | `stream.mp4` + `stream.packets` + `markers.json` |
| `canlapse.log` | startup / pick / record / encode diagnostics |
| `crash.log` | only if the app crashed (panic or unhandled exception) |

Both logs live next to the executable; a copy is also written to `%LOCALAPPDATA%\CanLapse\` for support.

Optional environment variables: `CANLAPSE_CACHE` (cache root), `CANLAPSE_LANG=en|zh` (force UI language,
mainly for testing).

---

## Build from source

Prerequisites: Rust (MSVC toolchain), Visual Studio Build Tools with the Windows SDK.
**The static ffmpeg/openh264 libraries are vendored** in `deps/ffmpeg-static/`, so a plain build produces
the same single-file executable — no external SDK required.

```powershell
# debug build + icon/version injection
pwsh -File tools\build.ps1

# release build (what ships)
pwsh -File tools\build.ps1 -Profile release

# optional Authenticode signing (Windows SDK signtool + certificate thumbprint)
pwsh -File tools\build.ps1 -Profile release -SignThumbprint <certificate-thumbprint>

# assemble dist\CanLapse-v<version>-win64.zip (exe + 使用说明.txt only)
pwsh -File tools\package.ps1 -Version 2.0.1

# tests
cargo test --offline
```

Notes:

- `.cargo/config.toml` sets `-C target-feature=+crt-static`; `build.rs` statically links
  `avformat/avcodec/swscale/swresample/avutil/openh264` plus the Windows system libraries, and finds the
  vendored libs automatically (override with `CANLAPSE_FFMPEG_STATIC`).
- Resource injection (icon + version info) happens **after** linking via `tools\embed-resources.ps1`;
  run the build and the injection script as a pair (that is what `build.ps1` does).
- If `-SignThumbprint` or `CANLAPSE_SIGN_THUMBPRINT` is set, the final binaries are signed after
  resource injection and verified with `Get-AuthenticodeSignature`. Without a certificate they remain unsigned.
- Rebuilding the static libraries themselves (only needed when bumping ffmpeg/openh264) is documented in
  [`deps/ffmpeg-static/README.md`](deps/ffmpeg-static/README.md).

### Project layout

```
src/                  Rust sources (ui/, capture.rs, recorder.rs, encode/, session.rs, lang.rs, …)
deps/ffmpeg-static/   vendored MSVC static libs + licenses + rebuild notes
tools/                build.ps1, package.ps1, embed-resources.ps1, ResInject.cs, 使用说明.txt
docs/evidence/        screenshots used in this README and in the logs
```

---

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Preview shows toolbars and layer panels | The canvas is not a floating window. Click **How to Locate** for per-app steps. |
| “Desktop composition (DWM) is off” | CanLapse tries direct capture instead. If the app does not support `PrintWindow`, check `canlapse.log`. |
| Target is mostly/fully outside the screen | CanLapse switches to direct `PrintWindow`; if that app does not support it, the reason is written to `canlapse.log`. |
| No frames at all | The canvas never changed during the recording — the session is discarded with a notice. |
| Video is short | Playback is fixed at 30 fps; `seconds = frames / 30`. Raise frame density for more frames. |

---

## Known limitations

- Canvas picking is manual by design (no auto-detection of “the canvas”).
- Direct capture still depends on the drawing app implementing `PrintWindow`; if it does not, the reason is written to `canlapse.log`.
- Streaming sessions are H.264-encoded as they record; finalization and re-export only remux packets.
- Legacy sessions containing `frame_*.jpg/png` remain readable and are encoded through the old offline path.
- Encoding remains single-threaded on a below-normal-priority worker thread.

- The floating bar position is not persisted between sessions (it re-snaps to the canvas monitor, top-right).

---

## Documentation

- [CHANGELOG.md](CHANGELOG.md) — version history (English + Chinese).
- [完工日志.md](完工日志.md) — Chinese completion/seal log: deliverables, verification matrix, build
  archaeology, per-version supplements (v1.1 i18n, v1.2 single-file).
- [项目总结.md](项目总结.md) — Chinese design notes: architecture, measured numbers, pitfalls.
- [使用说明.txt](tools/使用说明.txt) — bilingual end-user guide shipped in the zip.

---

## License

- **CanLapse source code**: MIT — see [LICENSE](LICENSE).
- **ffmpeg 9.0.2**: LGPL-2.1-or-later, statically linked (no GPL components enabled) —
  see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
- **OpenH264 2.6.0**: BSD-2-Clause, statically linked — see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

The release executable embeds LGPL-licensed ffmpeg code. If you redistribute the binary, keep this README
and the notices with it, and note that relinking against a modified ffmpeg is possible by following the
build instructions above.
