# Third-Party Notices

CanLapse itself is MIT-licensed (see [LICENSE](LICENSE)). The release executable **statically links**
the components below, so their notices must travel with any binary distribution.

---

## 1. FFmpeg 9.0.2

- **License**: GNU Lesser General Public License, version 2.1 or later (LGPL-2.1-or-later).
- **Build**: MSVC, `--enable-static --disable-shared`, **without** `--enable-gpl` and without
  any GPL component (no x264/x265, no `--enable-nonfree`). Only these parts are compiled in:
  `libavcodec`, `libavformat`, `libavutil`, `libswscale`, `libswresample`,
  the `libopenh264` encoder wrapper, the `mov`/`mp4` muxers and the `file` protocol.
- **Origin**: <https://ffmpeg.org/> — full license text: [`deps/ffmpeg-static/licenses/ffmpeg-LGPLv2.1.txt`](deps/ffmpeg-static/licenses/ffmpeg-LGPLv2.1.txt).
- **Modifications**: none to the source. Only the build configuration above was used.
- **Static linking & LGPL**: because ffmpeg is linked statically, LGPL §6 requires that recipients can
  relink the application against a modified version of the library. This is satisfied by the published
  sources and the documented, reproducible build:
  1. obtain the ffmpeg 9.0.2 sources (<https://ffmpeg.org/download.html>);
  2. rebuild with the exact `configure` line in [`deps/ffmpeg-static/README.md`](deps/ffmpeg-static/README.md);
  3. drop the resulting `*.lib` files into `deps/ffmpeg-static/lib/` and rebuild CanLapse with
     `pwsh -File tools\build.ps1 -Profile release`.
  The vendored static libraries in this repository are provided for exactly that purpose.

---

## 2. OpenH264 2.6.0

- **License**: BSD 2-Clause.
- **Usage**: H.264 encoder (`libopenh264` wrapper inside avcodec); statically linked.
- **Origin**: <https://github.com/cisco/openh264> —
  full license text: [`deps/ffmpeg-static/licenses/openh264-BSD.txt`](deps/ffmpeg-static/licenses/openh264-BSD.txt).
- **Note**: OpenH264's own binary-distribution policy concerns *Cisco's* prebuilt binaries;
  this project builds the library from source (see `deps/ffmpeg-static/README.md`).

---

## 3. Microsoft C/C++ Runtime (static CRT)

- The executable is built with `-C target-feature=+crt-static` and ffmpeg is compiled with the matching
  static CRT, so **no** `vcruntime140.dll` / `msvcp140.dll` is required at runtime.
- The statically linked C runtime is redistributable under the Visual Studio license terms
  (see "Microsoft Visual Studio 2022/2026 — Distributable Code").

---

## 4. Windows system libraries

The executable imports only operating-system DLLs (`kernel32`, `user32`, `gdi32`, `dwmapi`,
`comctl32`, `bcrypt`, `ntdll`, …). These are part of Windows and are **not** redistributed with CanLapse.

---

## 5. Rust crates

Compiled into the binary under their respective licenses:

| Crate | License | Purpose |
|---|---|---|
| `windows-sys` | MIT OR Apache-2.0 | Win32 API bindings |
| `image` | MIT OR Apache-2.0 | JPEG decoding (frame cache) |
| `png` | MIT OR Apache-2.0 | PNG encoding/decoding (blank frames) |
| `serde`, `serde_json` | MIT OR Apache-2.0 | session metadata (`markers.json`, `session.json`) |

Full texts can be obtained from <https://crates.io/> for the exact versions pinned in `Cargo.lock`.
