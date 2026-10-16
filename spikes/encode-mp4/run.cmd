@echo off
REM ===========================================================================
REM  Reproducible runner for the encode-mp4 spike.
REM  Offline Windows machine, rustc 1.99.0, target x86_64-pc-windows-msvc.
REM ===========================================================================
setlocal
cd /d "%~dp0"

REM --- 1. ffmpeg-min\bin must be on PATH so the av*.dll files resolve -------
REM --- 2. avcodec-63.dll ALSO needs libwinpthread-1.dll, which is NOT in
REM        ffmpeg-min\bin.  D:\CapLite\build holds a working copy (built
REM        against msvcrt.dll, like ffmpeg-min itself).  The copy in
REM        C:\mingw64\bin is a UCRT build and makes the loader abort with
REM        STATUS_ENTRYPOINT_NOT_FOUND (0xC0000139).  Verified, see FINDINGS.md.
set "PATH=D:\CapLite\ffmpeg-min\bin;D:\CapLite\build;%PATH%"

REM Optional: FFMPEG_MIN_DIR overrides the link search path baked into build.rs
REM           SPIKE_PROFILE=high  switches libopenh264 to High profile (CABAC)

echo === cargo build --offline --release ===
cargo build --offline --release
if errorlevel 1 exit /b 1

echo.
echo === run ===
target\release\encode-mp4-spike.exe out.mp4
if errorlevel 1 exit /b 1

echo.
echo === out.mp4 ===
dir /-c out.mp4
