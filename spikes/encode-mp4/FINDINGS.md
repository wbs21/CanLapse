# FINDINGS — 内存 BGRA 帧 → H.264 MP4（离线 Windows / Rust / `x86_64-pc-windows-msvc`）

本文件全部内容来自**真实执行**。凡是没实测的都在 §9 明确标注「未验证」。
原始日志留在同目录：`run.log`、`run_high.log`、`build_verbose.log`、`_probe/probe_out.txt`、
`_probe/ffmpeg-sys-probe/out.txt|out2.txt|out3.txt`。

---

## 0. 结论速览

| 问题 | 实测结论 |
|---|---|
| **1. 可用的 H.264 编码器** | **`libopenh264`，而且是唯一的视频编码器。** openh264 **2.6.0 静态编进了 `avcodec-63.dll`**，不依赖 `openh264.dll`。**顺带一个重要纠正：`h264` 解码器并没有被裁掉**，它就在同一个 DLL 里，所以能做真实解码回读验证。 |
| **2. msvc 下的链接做法** | **(b) 手写 `extern "C"` 声明 + MSVC 导入库直链**：`build.rs` 发 `-L native=D:\CapLite\ffmpeg-min\bin` 与 `-l dylib=avformat/avcodec/swscale/avutil`。`bin\*.lib` 是原生 x64 COFF 导入库，MSVC `link.exe` 直接吃下去了。 **(a) `ffmpeg-sys-next` + bindgen 三段实测失败，最后一关死在缺 `libclang`。** |
| **3. 产物** | `out.mp4`，**1,199,665 字节**，H.264 / `avc1` / 640×360 / 48 帧 / 4.800 s / 10 fps，稳定可复现（重跑字节级一致）。 |
| **验证强度** | ① 自写 MP4 box 解析；② **用 ffmpeg-min 自己的 `mov` demuxer + `h264` 解码器真实解回 48 帧、640×360、逐帧亮度与源帧 Pearson r = 0.99992**；③ 用完整 LGPL 构建的 `ffprobe.exe -count_frames` 交叉验证 `nb_read_frames=48`。 |

Rust 侧**零依赖**（`[dependencies]` 为空），因此 `cargo build --offline` 不触碰 registry，必然成功。

---

## 1. 环境（实测）

```
rustc 1.99.0 (b940084d8 2026-09-28)      cargo 1.99.0 (5f94df478 2026-08-27)
host target: x86_64-pc-windows-msvc（唯一已安装 target）
MSVC: Visual Studio 18 Community, link.exe 14.51.36260.0
      （link.exe / cl.exe 不在 PATH；cargo 自己找到了工具链，Rust 构建无需 vcvars）
ffmpeg-min: D:\CapLite\ffmpeg-min
  include/ libavcodec 63.1.102, libavformat 63.1.102, libavutil 61.1.102, libswscale 10.1.102
  bin/     avcodec-63.dll avformat-63.dll avutil-61.dll swscale-10.dll swresample-7.dll
           + avcodec.lib avformat.lib avutil.lib swscale.lib swresample.lib   ← MSVC 导入库在这里
  lib/     只有 *.def 与 MinGW lib*.dll.a（本次没用上）
网络: 不可用（static.crates.io SSL 失败）→ 全程 --offline
```

`ffmpeg-min\lib\pkgconfig\*.pc` 的 `prefix` 是坏的
（`C:UsersLIMITL~1DOCUME~1DEEPSE~1DEFAUL~1CapLiteffmpeg-min`，反斜杠被吞）。
**本次方案完全绕开 pkg-config**，不影响结论。

---

## 2. 问题 1：这套 ffmpeg-min 里实际可用的 H.264 编码器

### 2.1 直接枚举（`av_codec_iterate`，真实输出）

`run.log` 第 1 节（Rust 侧 `av_codec_iterate` 遍历，只注册了 4 个 codec）：

```
================ 1. codecs registered in avcodec-63.dll (av_codec_iterate) ================
  [ENCODER] type=AUDIO id=86018 name=aac              wrapper=-          caps=0x62 long_name=AAC (Advanced Audio Coding)
  [ENCODER] type=VIDEO id=27 name=libopenh264      wrapper=libopenh264 caps=0x108002 long_name=OpenH264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10
  [DECODER] type=VIDEO id=27 name=h264             wrapper=-          caps=0x3022 long_name=H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10
  [DECODER] type=AUDIO id=86018 name=aac              wrapper=-          caps=0x402 long_name=AAC (Advanced Audio Coding)
  total codecs registered: 4
```

**唯一视频编码器 = `libopenh264`**（`AV_CODEC_ID_H264 = 27`）。
**`h264` 解码器存在**（`caps=0x3022`），所以「只保留编码、去除解码」这个前提**部分不成立**——
编码确实只剩 openh264，但解码/解析/mov 解封装都还在。

### 2.2 编码器到底是静态编进 DLL 还是独立 DLL

**结论：静态编进 `avcodec-63.dll`，不存在对 `openh264.dll` 的依赖。**

证据链（全部实测）：

1. **依赖表**（`dumpbin /dependents avcodec-63.dll`）：

   ```
   KERNEL32.dll
   msvcrt.dll
   libwinpthread-1.dll
   avutil-61.dll
   ```
   → **没有 `openh264.dll`**。

2. **DLL 内部字符串/符号**：`avcodec-63.dll`（4,102,870 字节）里出现
   `libopenh264`（16 次）、`OpenH264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10`、
   `openh264enc.c`、`openh264_encoder`、`openh264_trace_callback`，
   以及 **3678 处 `Wels*` 符号**，包含 `WelsCreateSVCEncoder`、`WelsDestroySVCEncoder`、
   `WelsGetCodecVersion`、`WelsEnc19CWelsH264SVCEncoder11EncodeFrameEPK...` 等
   openh264 的内部 C++ 符号。

3. **构建配置**（`avcodec_configuration()` 真实输出）：

   ```
   --prefix='D:/tools/ffmpeg-min-new' --arch=x86_64 --target-os=mingw32 --enable-shared
   --disable-static --disable-programs --disable-doc --disable-debug --disable-autodetect
   --disable-network --enable-libopenh264 --pkg-config='D:/tools/wrappers/pkgconf-fit.exe'
   --disable-everything --enable-encoder='libopenh264,aac' --enable-decoder='h264,aac'
   --enable-parser='h264,aac' --enable-muxer='mp4,mov' --enable-demuxer='mov,mp4,matroska'
   --enable-protocol=file --enable-swscale --enable-swresample
   --extra-ldflags='-static-libstdc++ -static-libgcc' --extra-version=-capmin
   ```

   `--extra-ldflags='-static-libstdc++ -static-libgcc'` 也解释了为什么
   `avcodec-63.dll` 不依赖 `libstdc++-6.dll`。

### 2.3 「自编译最小方案」的出处：**不在 `D:\CapLite` 下，在 `D:\tools\`**

`D:\CapLite` 下**没有** openh264 的构建脚本；只 vendor 了一份源码 + 一份**独立 DLL**
（那是另一条路线，见 §2.4）。真正的出处在 `D:\tools\`：

| 路径 | 作用 | 实测内容 |
|---|---|---|
| `D:\tools\build-oh264.sh` | **openh264 构建脚本** | 头注释：「用 MinGW-w64 编译 openh264 **2.6.0** 静态库并安装到 `/d/tools/oh264-static`」；`make OS=mingw_nt libopenh264.a` → `make install-static` |
| `D:\tools\oh264-static\lib\libopenh264.a` | **真正被链进去的静态库** | 2,285,068 字节 |
| `D:\tools\oh264-static\lib\pkgconfig\openh264.pc` | 给 configure 看的 pc | `Version: 2.6.0`，`Libs: -L${libdir} -lopenh264 -lstdc++` |
| `D:\tools\oh264-src\` | openh264 **2.6.0 源码树** | `meson.build`: `version : '2.6.0'`；含 `.git`、`codec/`、`module/`、`test/`、`LICENSE` |
| **`D:\tools\wrappers\pkgconf-fit.c`** | **决定性的一环** | 22 行 C，注释：「pkg-config 包装器，为 FFmpeg configure 注入 openh264 静态库搜索路径（Git Bash 启动的 msys64 程序会丢掉 `PKG_CONFIG_PATH`，故用 `--with-path` 显式传入）」，代码里硬编码 `na[1] = "--with-path=D:\\tools\\oh264-static\\lib\\pkgconfig"` |
| `D:\tools\ffmpeg-min-new\` | configure 里的 `--prefix` | `bin\avcodec-63.dll` 也是 4,102,870 字节，与 `D:\CapLite\ffmpeg-min\bin\` 同一份构建 |

也就是说：**openh264 2.6.0 用 MinGW 编成 `libopenh264.a`，靠 `pkgconf-fit.exe` 骗过
FFmpeg configure 的 pkg-config 探测，被静态链进 `avcodec-63.dll`。**

### 2.4 另外确实存在一个独立的 openh264 DLL（但本方案没用它）

```
D:\CapLite\includes\openh264\openh264.dll     823,296 B
D:\CapLite\includes\openh264\openh264.lib       2,922 B   ← MSVC 导入库
```

`dumpbin /exports` 实测（**标准 OpenH264 C API，共 6 个导出**）：

```
ordinal hint RVA      name
      1    0 00063A30 WelsCreateDecoder
      2    1 0002BF30 WelsCreateSVCEncoder
      3    2 00063C10 WelsDestroyDecoder
      4    3 0002BFD0 WelsDestroySVCEncoder
      5    4 0002BFF0 WelsGetCodecVersion
      6    5 0002C000 WelsGetCodecVersionEx
```

依赖：`KERNEL32.dll VCRUNTIME140.dll VCRUNTIME140_1.dll api-ms-win-crt-*`
→ **这是 MSVC 构建的共享库**（不是 MinGW）。
源码树在 `D:\CapLite\includes\openh264\openh264\`（有 `build_x64\meson-private\openh264.pc`，
`Version: 2.6.0`；`LICENSE` 为 Cisco BSD 二条款）。

> 所以「直接调 `WelsCreateSVCEncoder` 出 Annex-B 裸流 + 自己封 MP4」**技术上可行**，
> 而且 `D:\CapLite\includes\Bento4\`（`ap4.lib` + 源码）和 `libyuv`（`yuv.lib`）都在，
> 足以绕开整个 FFmpeg。**但本次没有走这条路**，因为它要自己实现 MP4 封装、
> 时间戳/`stss`/`avcC` 生成，工作量和风险都远大于方案 (b)。**该路线标注「未验证」。**

---

## 3. 问题 2：msvc target 下链接这套 ffmpeg 的可行做法

### ✅ 采用的方案：(b) 手写 `extern "C"` + MSVC 导入库直链

**结论：`ffmpeg-min\bin\*.lib` 是原生 x64 COFF 导入库，MSVC `link.exe` 能直接吃，不需要 MinGW。**

验证方式 1 —— 文件格式（`dumpbin /headers D:\CapLite\ffmpeg-min\bin\avcodec.lib`）：

```
File Type: LIBRARY
FILE HEADER VALUES
            8664 machine (x64)
```

（`lib\libavcodec.dll.a` 也是 x64 COFF LIBRARY，但**本次完全没用到 `lib\`**。）

验证方式 2 —— 实际链接命令行。`cargo build --offline --release -v` 抓到的 rustc 调用：

```
Running `...\rustc.exe --crate-name encode_mp4_spike --edition=2021 src\main.rs
  --crate-type bin --emit=dep-info,link -C opt-level=2
  -L dependency=...\target\release\deps
  -L native=D:\CapLite\ffmpeg-min\bin
  -l dylib=avformat -l dylib=avcodec -l dylib=swscale -l dylib=avutil`
    Finished `release` profile [optimized] target(s) in 2.04s
```

验证方式 3 —— 生成的 exe 导入表（`dumpbin /dependents`）：

```
avformat-63.dll
avcodec-63.dll
swscale-10.dll
avutil-61.dll
... VCRUNTIME140.dll, KERNEL32.dll, api-ms-win-crt-*  (Rust/MSVC 自带)
```

**链接配置全部在 `build.rs`，不需要 `.cargo/config.toml`，也不需要 vcvars：**

```rust
let root = env::var("FFMPEG_MIN_DIR").unwrap_or_else(|_| r"D:\CapLite\ffmpeg-min".to_string());
let bin  = PathBuf::from(&root).join("bin");
println!("cargo:rustc-link-search=native={}", bin.display());
for lib in ["avformat", "avcodec", "swscale", "avutil"] {   // 依赖者在前
    println!("cargo:rustc-link-lib=dylib={lib}");
}
```

**结构体布局怎么办？** 只手写真正用到的字段，**偏移量靠实测而不是猜**：
`_probe/ffprobe_min.c` 用**同一套头文件**、由**本机 MSVC `cl.exe`** 编译，
打印 `offsetof()` / `sizeof()`；原始输出在 `_probe/probe_out.txt`。
因为 `cl.exe` 与 `rustc --target x86_64-pc-windows-msvc` 是同一套 ABI，偏移量可直接搬。
Rust 里所有 FFmpeg 对象都是不透明的 `*mut c_void`，通过 `ffi::off` 常量 + 带类型的读写函数访问。

实测到的关键偏移（`AVCodecContext` sizeof 864 / `AVFrame` 424 / `AVPacket` 104 /
`AVStream` 216 / `AVCodecParameters` 184 / `AVFormatContext` 480 / `AVCodec` 56）：

```
AVCodecContext: priv_data 32, bit_rate 56, flags 64, extradata 72, extradata_size 80,
                time_base 84, framerate 100, delay 108, width 112, height 116,
                sample_aspect_ratio 128, pix_fmt 136, sw_pix_fmt 140, refs 168,
                max_b_frames 200, gop_size 332, rc_buffer_size 448, rc_max_rate 464,
                thread_count 656, profile 688, level 692
AVFrame:        data 0 [8×ptr], linesize 64 [8×int], extended_data 96, width 104, height 108,
                format 116, pts 136
AVPacket:       pts 8, dts 16, data 24, size 32, stream_index 36, flags 40, duration 64
AVStream:       index 8, codecpar 16, time_base 32, duration 48, avg_frame_rate 88, r_frame_rate 204
AVCodecParameters: codec_id 4, extradata 16, extradata_size 24, format 44, width 72, height 76
AVFormatContext: pb 32, nb_streams 44, streams 48
```

> **踩过的坑（已修）**：`AVFormatContext.streams` 是 `AVStream **`（指向数组的指针），
> **不是内联数组**。我先用「读内联指针数组」的辅助函数 `arr_ptr(ic, streams, 0)`，
> 结果拿到的是**数组基址**而不是 `streams[0]`，接着把它当 `AVStream*` 解引用 → `0xC0000005`。
> 正确做法是解引用两次。`AVCodecContext.extradata` 这类才是内联字段。

### ❌ (a) `ffmpeg-sys-next` + `ffmpeg-next`：三段实测失败，最后一关是 **libclang 缺失**

先回答「本机有没有 `libclang.dll`」：**没有。**
在下面这些根目录递归搜 `libclang*.dll` 全部为 0 命中：

```
C:\Python314   C:\Program Files   C:\Program Files (x86)
C:\Users\Limit li\.cargo          C:\Users\Limit li\.rustup
C:\msys64      C:\mingw64         D:\tools\msys64
D:\CapLite\includes               C:\Qt     D:\Qt
C:\Strawberry  C:\ProgramData
```
也没有 LLVM/clang 安装目录，也没有 Python 的 clang 包。（`clang++` 不在 PATH。）

**实测 1**：单独一个 `bindgen = "0.73.2"` 探针工程，`cargo build --offline`：

```
error: no matching package named `prettyplease` found
location searched: crates.io index
required by package `bindgen v0.73.2`
```
（缓存只有 243 个 `.crate`，`prettyplease` 不在其中；注意 `ffmpeg-sys-next` 用
`bindgen features=["runtime"], default-features=false`，所以它不会碰到 `prettyplease`。）

**实测 2**：`ffmpeg-sys-next = "9.0.1"` 默认 features，`cargo build --offline`：

```
--- stderr
error: FFmpeg not found.

FFmpeg libraries could not be found using any of the following methods:
  1. FFMPEG_DIR environment variable (not set)
  2. vcpkg package manager (ffmpeg package not found)
  3. pkg-config (libavutil not found: Could not run `pkg-config --libs --cflags libavutil`
The pkg-config command could not be found.
```
→ 本机没有 `pkg-config` 可执行文件，`ffmpeg-min` 的 `.pc` 前缀又是坏的，**这条直接死**。

**实测 3**：补上 `FFMPEG_DIR=D:\CapLite\ffmpeg-min`（版本探测终于通过）：

```
D:\CapLite\ffmpeg-min\include\libavformat/avformat.h(1092): warning C4201 ...
check.c(771): fatal error C1083: 无法打开包括文件: "libavfilter/avfilter.h": No such file or directory
thread 'main' panicked at ffmpeg-sys-next-9.0.1\build.rs:919:9:  Compile failed
```
→ 默认 features 包含 `avfilter`/`avdevice`，而 min 构建 `--disable-everything` 没有它们。
需要 `default-features = false, features = ["avcodec","avformat","swscale"]`。

**实测 4**（把 features 收窄后，终于走到 bindgen）：

```
cargo:rustc-cfg=feature="ffmpeg_9_0"        ← 版本探测成功
--- stderr
thread 'main' (19236) panicked at C:\Users\Limit li\.cargo\registry\src\
index.crates.io-1949cf8c6b5b557f\bindgen-0.73.2\lib.rs:618:27:
  Unable to find libclang: "couldn't find any valid shared libraries matching:
  ['clang.dll', 'libclang.dll'], set the `LIBCLANG_PATH` environment variable
  to a path where one of these files can be found (invalid: [])"
```

→ **这就是「缺 libclang 导致失败」的确切证据。**
完整失败记录（含原始日志）留在 `_probe/ffmpeg-sys-probe/`：
`out.txt`（无 FFMPEG_DIR）、`out2.txt`（有 FFMPEG_DIR + 默认 features）、`out3.txt`（收窄 features → libclang）。

**结论**：方案 (a) 在本机**不可用**，除非先离线搞到
`libclang.dll`（或装 LLVM）+ 补齐缺失的 crate。

### 其他方案的可行性评估

- **(c) `libloading` 运行时动态加载**：`libloading-0.8.9` 在缓存里，**理论上完全可行**
  （DLL 导出的是标准 C 符号，无 name mangling 问题），而且这正是**上一个产品 CapLite 的做法**
  ——`D:\CapLite\README.md` 明确写着「FFmpeg 仅在运行时 `LoadLibrary`，不静态链接，以保持 LGPL 合规」，
  参考实现在 `D:\CapLite\src\ffmpeg\FfmpegSymbols.h` / `FfmpegApi.cpp`。
  **本次没有实测这条路**（标注「未验证」）。它不解决结构体偏移问题（`libloading` 只是
  把「链接期」的活挪到「运行期」），所以对本次目标没有额外收益；
  但它对 LGPL 合规与「DLL 缺失时优雅降级」有实际价值，**建议正式项目认真考虑**。
- **(d) 其他**：
  - C shim（`build.rs` 里调 gcc/clang 编一个薄 C 层）：可行但引入 C 工具链依赖，本机
    `cl.exe` 需要 vcvars、`gcc` 是 MinGW（会牵扯 msvcrt/UCRT 混用，见 §5），**未实测**。
  - 独立 `openh264.dll` + 自写 MP4 封装（或 Bento4 `ap4.lib`）：见 §2.4，**未实测**。

---

## 4. 问题 3：48 帧 640×360 BGRA @10fps → `out.mp4`（实测通过）

### 4.1 精确复现步骤

```bat
:: 前置：工作目录 = D:\deepseek-harness\default-workspace\CanLapse\spikes\encode-mp4
:: 需要的环境变量只有 PATH：
set "PATH=D:\CapLite\ffmpeg-min\bin;D:\CapLite\build;%PATH%"
::   D:\CapLite\ffmpeg-min\bin  → av*-63/61/10.dll 与 *.lib
::   D:\CapLite\build           → libwinpthread-1.dll（必需，见 §5）
:: 可选：
::   set FFMPEG_MIN_DIR=D:\CapLite\ffmpeg-min   （build.rs 的链接搜索路径，默认就是这个）
::   set SPIKE_PROFILE=high                     （libopenh264 用 High profile/CABAC）
::   set SPIKE_VERBOSE=0                        （关掉逐帧/逐包刷屏）

cargo build --offline --release
target\release\encode-mp4-spike.exe out.mp4
```

也可以直接跑 `run.cmd`（内容同上）。

`Cargo.toml` 关键内容（**零依赖**）：

```toml
[package]
name = "encode-mp4-spike"
version = "0.1.0"
edition = "2021"
build = "build.rs"

[[bin]]
name = "encode-mp4-spike"
path = "src/main.rs"

[dependencies]        # 故意为空：--offline 不会碰 registry，也就不可能失败

[workspace]           # 隔离，避免被上层 workspace 吞掉
```

### 4.2 关键命令输出（`run.log`）

```
================ 0. identity ================
  rust target   : x86_64
  output file   : out.mp4
  avcodec_configuration():
    --prefix='D:/tools/ffmpeg-min-new' ... --enable-libopenh264 --disable-everything
    --enable-encoder='libopenh264,aac' --enable-decoder='h264,aac' ...

================ 1. codecs registered in avcodec-63.dll (av_codec_iterate) ================
  [ENCODER] type=VIDEO id=27 name=libopenh264      wrapper=libopenh264 caps=0x108002 ...
  [DECODER] type=VIDEO id=27 name=h264             wrapper=-          caps=0x3022 ...
  total codecs registered: 4

================ 2. encode 48 synthetic BGRA frames (640x360 @ 10fps) -> H.264 MP4 ================
  encoder found: name=libopenh264 long_name=OpenH264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 wrapper=libopenh264
  fields written directly at measured offsets:
    width=640 height=360 pix_fmt=0 time_base=1/10 framerate=10/1 gop_size=10 max_b_frames=0
    bit_rate=2000000 flags=0x400000 (GLOBAL_HEADER set = true)
  AVOptions (these DO exist on AVCodecContext):
  av_opt_set_int("b", 2000000, flags=0) -> 0 OK
  av_opt_set_int("g", 10, flags=0) -> 0 OK
  av_opt_set_int("bf", 0, flags=0) -> 0 OK
  av_opt_set_int("threads", 1, flags=0) -> 0 OK
  libopenh264 wrapper private options (set on ctx->priv_data, offset 32):
  ctx->priv_data = 0x...
  SPIKE_PROFILE = constrained_baseline
  av_opt_set("profile" = "constrained_baseline", flags=0) -> 0 OK
  av_opt_set("rc_mode" = "bitrate", flags=0) -> 0 OK
  av_opt_set_int("allow_skip_frames", 0, flags=0) -> 0 OK
  avcodec_open2 OK
  context after open: width=640 height=360 pix_fmt=0 sw_pix_fmt=-1 time_base=1/10 extradata_size=28
  sws_getContext(BGRA(28) -> YUV420P(0)) OK
  av_frame_get_buffer OK: Y linesize=640 U linesize=320 V linesize=320
  stream codecpar: codec_type=0 codec_id=27 width=640 height=360 format=0 bit_rate=2000000 extradata_size=28
  avformat_write_header OK: requested st.time_base=1/1000, muxer chose 1/16000
  frame  0: bgra fnv1a=0x37d82d42e1961265 luma_mean=113.096
    packet: size=  7377 pts=    0 dts=    0 duration=   0 key=true (rescaled 1/10 -> 1/16000)
  frame  1: bgra fnv1a=0x0ef2b062e8928625 luma_mean=113.741
    packet: size= 14814 pts= 1600 dts= 1600 duration=   0 key=false (rescaled 1/10 -> 1/16000)
  ... (48 帧全不同：fnv1a 哈希 48 个互不相同)
  frame 47: bgra fnv1a=0xab5c6a07213a6ee5 luma_mean=117.938
    packet: size= 23892 pts=75200 dts=75200 duration=   0 key=false (rescaled 1/10 -> 1/16000)
  frames submitted            : 48
  distinct source frame hashes: 48
  H.264 access units written  : 48
  total elementary-stream data: 1198598 bytes
```

### 4.3 我自己写的 MP4 box 解析结果（`run.log` 第 3 节）

```
  file size: 1199665 bytes
  top-level boxes: ftyp, free, mdat, moov
  ftyp=true major_brand=isom compatible=["isom", "iso2", "avc1", "mp41"]
  moov=true mdat=true mdat_bytes=1198598 mdat_boxes=1
  mvhd timescale=1000 duration=4800 => 4.800 s
  streams found: 1
    trak[0]: handler=vide fourcc=avc1 640x360 timescale=16000 duration=76800 (4.800 s)
             samples=48 sample_bytes=1198598 avg_fps=10.000 sync_samples=Some(5) chunks=Some(2)
  --- full box tree ---
    moov
      mvhd  version=0 timescale=1000 duration=4800
      trak
        tkhd / edts(elst)
        mdia
          mdhd  version=0 timescale=16000 duration=76800
          hdlr  handler_type=vide
          minf
            vmhd / dinf(dref)
            stbl
              stsd  entry_count=1
                avc1  fourcc=avc1 width=640 height=360
                  avcC  configurationVersion=1 AVCProfileIndication=66 profile_compatibility=192
                        AVCLevelIndication=22 lengthSizeMinusOne=3 numOfSPS=1
                  pasp / btrt
              stts  time_to_sample_entries=1  entry[0] count=48 delta=1600
              stss  sync_sample_count=5
              stsc
              stsz  sample_size=0 sample_count=48 total_sample_bytes=1198598
              stco  chunk_count=2
      udta
        meta
          hdlr  handler_type=mdir
          ilst
```

→ `ftyp`/`moov`/`mdat` 齐备、`mdat` 非空（1,198,598 B）、**1 条视频轨道、
48 个 sample、5 个关键帧、时长 4.800 s**、`avcC` 说明确是 H.264。

### 4.4 真实解码回读（用 ffmpeg-min 自己的 `h264` 解码器）

```
================ 4. real decode-back: ffmpeg-min mov demuxer + ffmpeg-min h264 decoder ================
  demuxed nb_streams=1
  ic->streams (AVStream **) = 0x...
    stream[0] AVStream* = 0x...
    stream[0] codecpar = 0x...
    stream[0] codec_id=27 codec_type=0 640x360 pix_fmt=0 extradata_size=31 time_base=1/16000
  av_find_best_stream picked decoder: h264
  chosen stream[0] AVStream* = ...
  chosen stream codecpar = ...
  avcodec_alloc_context3(h264) = ...
  avcodec_parameters_to_context OK
  avcodec_open2(decoder) OK
  decoder open: width=640 height=360 pix_fmt=0
  first decoded frame: 640x360 format=0 linesize[0]=640
  demuxed packets: 48
  DECODED FRAMES : 48
  decoded size   : 640x360 pix_fmt=0
  decoded Y mean : min=112.998 max=134.515 spread=21.517

================ 5. verdict ================
  encoder       : libopenh264 (statically inside avcodec-63.dll)
  mp4 muxer     : libavformat 'mp4'
  file          : out.mp4
  mvhd duration : 4.800 s @ timescale 1000
  video track   : avc1 640x360 samples=48 sync=Some(5) bytes=1198598
  decoded back  : 640x360 pix_fmt=0 frames=48
  source BGRA luma means : [113.1, 113.7, 114.4, ... 119.2, 117.9]
  decoded Y means        : [113.0, 113.5, 114.2, ... 119.0, 117.7]
  max |decoded-expected| = 0.227   pearson r = 0.99992

  *** SPIKE PASSED: BGRA frames -> H.264 MP4 -> decoded back consistently ***
```

> **这不是「box 结构看起来对」而已**：48 帧是**真解出来了**，分辨率正确，
> 而且**逐帧亮度与源 BGRA 帧的逐帧亮度几乎完全一致**
> （最大偏差 0.227 灰阶，Pearson r = **0.99992**），说明帧序、帧数、时间对应关系都对。

### 4.5 用完整 LGPL 构建独立交叉验证

```
> D:\CapLite\ffmpeg-n9.0-latest-win64-lgpl-shared-9.0\bin\ffprobe.exe ^
    -v error -show_streams -show_format -count_frames out.mp4

[STREAM]
index=0
codec_name=h264
profile=Constrained Baseline
codec_tag_string=avc1
mime_codec_string=avc1.42c016
width=640
height=360
coded_width=640
coded_height=368
has_b_frames=0
pix_fmt=yuv420p
level=22
is_avc=true
nal_length_size=4
r_frame_rate=10/1
avg_frame_rate=10/1
time_base=1/16000
duration=4.800000
bit_rate=1997663
nb_frames=48
nb_read_frames=48
extradata_size=31
[/STREAM]
[FORMAT]
format_name=mov,mp4,m4a,3gp,3g2,mj2
start_time=0.000000
duration=4.800000
size=1199665
bit_rate=1999441
probe_score=100
TAG:major_brand=isom
TAG:encoder=Lavf63.1.102
[/FORMAT]

> ffprobe -select_streams v:0 -show_entries packet=pts_time,dts_time,duration_time,size,flags -of csv out.mp4
packet,0.000000,0.000000,0.100000,7377,K__
packet,0.100000,0.100000,0.100000,14814,___
packet,0.200000,0.200000,0.100000,20525,___
packet,0.300000,0.300000,0.100000,23182,___
packet,0.400000,0.400000,0.100000,28618,___

> ffmpeg.exe -v error -i out.mp4 -f null -
(无输出，exit 0 = 全片解码无 error/warning)
```

`nb_read_frames=48` 是完整构建**真解了 48 帧**数出来的，与 §4.4 用 ffmpeg-min 解码器
得到的结果一致。

### 4.6 产物

| 文件 | 大小 | SHA256 |
|---|---|---|
| `out.mp4`（默认 `constrained_baseline`） | **1,199,665 B** | `4059C635F7C478BF97BE6896B750BD1A9FE7EE082F8E21EF7CEDCC5FB8D7A9CF` |
| `out_high.mp4`（`SPIKE_PROFILE=high`） | 1,122,053 B | `828FE4B60959AD309E6A7722D0B99DC7C06A6B2CDED4AAB1C7901E54F810087D` |

重跑字节级一致（同一 SHA256），说明确定性。

`SPIKE_PROFILE=high` 实测也通过（48 帧解码回读，`max |decoded-expected| = 0.187`，
`pearson r = 0.99996`），`ffprobe` 报 `profile=High`，体积小 6.5%，
**而且消掉了下面 §6 里那条 openh264 profile 警告**——因为 High 是 openh264 原生支持的 profile。

---

## 5. 运行时 DLL 部署（实测出来的坑，务必看）

`avcodec-63.dll` 的依赖里有 **`libwinpthread-1.dll`，而 `D:\CapLite\ffmpeg-min\bin\` 里没有它。**
这非常容易踩。实测（每次都重新启动进程、真实退出码）：

| `PATH` 里的 ffmpeg DLL 目录 + libwinpthread 来源 | 退出码 | 含义 |
|---|---|---|
| `ffmpeg-min\bin` + `D:\CapLite\build` | **0** | ✅ 正常 |
| `ffmpeg-min\bin` + `D:\tools\msys64\mingw64\bin` | **0** | ✅ 正常（这就是编译 ffmpeg-min 的那套工具链） |
| `ffmpeg-min\bin` + `C:\mingw64\bin` | **-1073741511 (0xC0000139)** | ❌ `STATUS_ENTRYPOINT_NOT_FOUND` |
| `ffmpeg-min\bin` + `C:\mingw64\bin` + `D:\CapLite\build` | **-1073741511 (0xC0000139)** | ❌ 先命中的那个赢了 |
| `ffmpeg-min\bin`（任何地方都没有 libwinpthread） | **-1073741515 (0xC0000135)** | ❌ `STATUS_DLL_NOT_FOUND` |

三个 `libwinpthread-1.dll` 是**三个不同的文件**：

```
1BBD639C...C15408  D:\CapLite\build\libwinpthread-1.dll                 66,467 B  (msvcrt 系)  ✅
C2652820...F1417E  D:\tools\msys64\mingw64\bin\libwinpthread-1.dll              ✅
0836F647...C93B51  C:\mingw64\bin\libwinpthread-1.dll                   55,808 B  (UCRT 系)   ❌
```

`dumpbin /dependents` 显示 `D:\CapLite\build` 那份依赖 `msvcrt.dll`，
而 `C:\mingw64\bin` 那份依赖 `api-ms-win-crt-*`（UCRT）。
ffmpeg-min 自身依赖的是 `msvcrt.dll`——**工具链 CRT 系不一致**，所以 UCRT 那份会在装载期炸。

> **结论：正式项目必须把 `D:\CapLite\build\libwinpthread-1.dll`（或
> `D:\tools\msys64\mingw64\bin\libwinpthread-1.dll`）跟 exe 一起发，或者放到 exe 同目录，
> 绝不能指望运行机器 PATH 里恰好有对的那一份。**
> 另外我在沙箱里验证过：把 3 份 libwinpthread 分别放到 exe 同目录 + 同一目录里放 ffmpeg DLL，
> **msys64 与 CapLite\build 两份都能跑通**。

---

## 6. 走过的弯路 / 失败记录

1. **`av_opt_set(ctx, "rc_mode", ..., AV_OPT_SEARCH_CHILDREN)` 返回
   `-1414549496` (`AVERROR_OPTION_NOT_FOUND`)** —— `AVCodecContext` 的 AVClass 在这套构建里
   没有通过 `child_next` 暴露 wrapper 的 `priv_class`。
   **解法**：直接写 `ctx->priv_data`（实测偏移 **32**）：
   `av_opt_set(priv_data, "rc_mode", "bitrate", 0)` → 0。
2. **`av_opt_set_int(ctx, "width"/"height"/"pix_fmt"/"framerate"/"gop_size"/"max_b_frames", ...)`
   全部 `AVERROR_OPTION_NOT_FOUND`**。只有 `time_base`/`b`/`g`/`bf`/`threads` 是 AVOption。
   **所以 width/height/pix_fmt/framerate/gop_size/max_b_frames 必须走结构体偏移。**
   （这正是「不能只靠 av_opt 绕开布局」的实测依据。）
3. **`avformat_write_header` 会改写 `st->time_base`**：请求 `1/1000`，mov muxer 实际选
   **`1/16000`**（头文件里也写明了这一点：「In avformat_write_header(), the muxer will
   overwrite this field with the timebase that will actually be used」）。
   我一开始按请求值 `1/1000` 做 `av_packet_rescale_ts`，结果文件时长只有 **0.394 s**（短了 16 倍），
   断言直接失败。
   **解法**：`avformat_write_header` **之后**再读 `st->time_base` 作为 rescale 目标。
4. **`AVFormatContext.streams` 是 `AVStream **` 不是内联数组**，用错辅助函数会拿到数组基址，
   随后当 `AVStream*` 解引用 → **`0xC0000005` 访问违例**（进程直接崩，日志停在上一行）。
5. **自己写的 C 探针里 `av_opt_next((void*)cls, o)` 崩溃**：`av_opt_next` 的第一个参数是
   「对象/类指针的地址」，必须传 `&cls`，不是 `cls`。
6. **完整的 ffmpeg LGPL 构建用不了来做「是不是只有 openh264」的判断**——那是另一个
   `avcodec-63.dll`（91 MB，含 x264 等）。所以编码器枚举必须用 ffmpeg-min 自己的 DLL 来做。
7. 写 MP4 解析器时把 `udta` 当成「full box 容器」（多跳了 4 字节），报
   `<malformed box header for '    '>`；`udta` 是普通容器，`meta` 才是 full box。
8. `%errorlevel%` 在 `cmd /c "A & echo RC=%errorlevel%"` 单行里是**解析期展开**的，
   我第一次做 DLL 隔离测试时因此读到了永远为 0 的「成功」，差点得出错误结论。
   重测后才是 §5 那张表。

**环境层面已确认但没花时间的地方**：`ffmpeg-min\lib\pkgconfig\*.pc` 前缀是坏的（已知），
所以方案 (a) 在 pkg-config 这一步就必然死，我没有去修那些 `.pc`。

---

## 7. 为了跑通这个 spike 临时绕过/简化了什么

这些是 spike 的取舍，**搬进正式项目前要重新审视**：

1. **`build.rs` 里硬编码了 `D:\CapLite\ffmpeg-min`**（可用 `FFMPEG_MIN_DIR` 覆盖）。
   正式项目应改成从 repo 内相对路径或构建配置读取。
2. **依赖 `PATH` 找 DLL**，没有把 DLL 复制到 exe 同目录，也没有做 LoadLibrary 兜底。
3. **只手写了用到的结构体字段偏移**（§3 那张表），没有生成完整绑定。
   一旦 FFmpeg 升级到 n10/avcodec 64，**必须重新跑 `_probe/ffprobe_min.c` 重新取偏移**。
   代码里已把偏移集中放在 `src/ffi.rs` 的 `pub mod off` 并注明出处。
4. **单线程**（`thread_count=1`）+ `max_b_frames=0` + `gop_size=10` + `bit_rate=2 Mbps`，
   没有做码率/质量调参，也没有 B 帧、没有 lookahead。
5. **合成帧只有 48 张、640×360**，不做缩放（`sws_getContext` 源/目标同尺寸）。
   正式项目要处理任意分辨率与 BGRA/BGRX 变体。
6. **`allow_skip_frames = 0`** 是刻意选的：时间 lapse 编码不能丢帧。
   代价是 openh264 会警告「bitrate can't be controlled ... without enabling skip frame」
   （实测文件码率 1,997,663 bps ≈ 目标 2 Mbps，所以码率实际是受控的）。
7. **默认 profile 用 `constrained_baseline`**（最大兼容性）。这条会触发 openh264 的
   `Warning:layerId(0) doesn't support profile(578), change to UNSPECIFIC profile`
   —— 因为 FFmpeg 的 wrapper 把 `AV_PROFILE_H264_CONSTRAINED_BASELINE`（实测值 **578**）
   原样传给 openh264，而 openh264 的 `EProfileIdc` 里没有 578。
   它只是警告、会退回 UNSPECIFIC，产出仍是合法的 baseline（`avcC` profile=66）。
   **想消掉它就用 `SPIKE_PROFILE=high`（实测通过）。**
8. **`avformat_open_input` 用 `fmt=null`**，靠探测；正式项目建议显式指定 demuxer。
9. **没有处理 `AVERROR_EXIT` / 中断**，也没有给 `avcodec_open2` 传 `AVDictionary` 选项。

---

## 8. 搬进 `CanLapse\src\` 要注意的事

### 8.1 链接方式

- 方案 (b)（`#[link]`/`-l dylib=` + 导入库）**实测可行**，而且 `cargo build --offline` 全自动。
  代价：编译期就绑定到 `D:\CapLite\ffmpeg-min`，**DLL 缺失时 exe 根本起不来**（装载期失败，
  没有优雅降级）。
- **上一个产品 CapLite 走的是运行时 `LoadLibrary`（方案 c）**，README 的理由是 LGPL 合规：
  「FFmpeg 仅在运行时 `LoadLibrary`，不静态链接，以保持 LGPL 合规」。
  参考实现：`D:\CapLite\src\ffmpeg\FfmpegSymbols.h`、`FfmpegApi.cpp`、
  `D:\CapLite\tools\record_probe_console.cpp`。
  → **正规做法建议照抄这个模式**：符号表 + `LoadLibraryW`，
  这样 (i) 满足 LGPL「可替换库」的要求，(ii) 少了 ffmpeg 时能给出友好提示而不是崩在装载期。
  本次 spike **没有实测 (c)**，只实测了 (b)；但如果 (b) 能跑通，(c) 只是把同一批函数指针
  从「装载期解析」改成「运行期 `GetProcAddress`」，风险主要在符号名和结构体偏移（同一套）。
- 无论走哪条，**结构体偏移问题都躲不掉**（除非拿到 bindgen/libclang）。

### 8.2 DLL 部署清单（必须一起发）

```
avcodec-63.dll
avformat-63.dll
avutil-61.dll
swscale-10.dll            （本次实际用到的 4 个）
swresample-7.dll          （纯视频可不要；音频编码需要）
libwinpthread-1.dll       ← 必须显式带上！来源见 §5
```
`ffmpeg-min\bin\` 里**没有** `libwinpthread-1.dll`，这一点最容易漏。
`D:\CapLite\build\` 与 `D:\tools\msys64\mingw64\bin\` 各有一份可用的；
**不要**用 `C:\mingw64\bin` 那份（UCRT，实测 0xC0000139）。

### 8.3 libopenh264 的可用性与授权（重要）

- **可用性**：`libopenh264` 是这套 ffmpeg-min **唯一**的 H.264 编码器（`av_codec_iterate` 实测），
  已静态编进 `avcodec-63.dll`，**不依赖 `openh264.dll`**。
  所以只要 `avcodec-63.dll` 在位，编码能力就在位，不需要额外发 `openh264.dll`。
- **版本**：openh264 **2.6.0**（`D:\tools\oh264-src\meson.build`、`D:\tools\oh264-static\*.pc`）。
- **许可结构**：源码是 **Cisco BSD 二条款**（`D:\CapLite\includes\openh264\openh264\LICENSE`）。
  但 **H.264 的专利授权是另一回事**：Cisco 的公开承诺只覆盖
  **「从 Cisco 官方渠道下载的二进制」**（Cisco 代付 MPEG LA 版税）。
  **本项目是把 openh264 自己编成静态库再用**（`D:\tools\build-oh264.sh`），
  **不属于 Cisco 代付范围 → 版税责任落在发布方身上。**
  → 这是**必须让产品/法务确认**的事，尤其如果 CanLapse 要对外分发。
  （`D:\CapLite\README.md` 只提到 LGPL 合规，没有提到 H.264 版税，可能是漏项。）
- **FFmpeg 侧的许可**：ffmpeg-min 是 `--enable-shared`，
  链接 `avcodec-63.dll`/`avformat-63.dll` 属于 LGPL 允许的动态链接。
  但 `--enable-libopenh264` 在 FFmpeg 里算 external library，
  **本次构建的 `avcodec_configuration()` 里没有 `--enable-gpl`、没有 `--enable-nonfree`**，
  所以 FFmpeg 本身仍可按 **LGPL v2.1+** 分发（需附 LGPL 文本、提供对应源码/构建脚本）。
  构建脚本已经留在 `D:\tools\build-oh264.sh` / `ffmpeg-build.ps1` / `pkgconf-fit.c`，这是好事。

### 8.4 其他工程注意点

- **偏移表必须随 FFmpeg 版本走**：把 `src/ffi.rs` 的 `off` 模块与
  `_probe/ffprobe_min.c` 一起纳入仓库，升级 ffmpeg-min 时重跑探针并 diff。
- **`st->time_base` 必须在 `avformat_write_header` 之后再读**（§6.3），
  否则时长会错 16 倍——这是最容易在生产里变成「视频快放/慢放」的坑。
- **`AV_CODEC_FLAG_GLOBAL_HEADER` 必须在 `avcodec_open2` 之前设**，
  否则 mp4 muxer 拿不到 `extradata`（SPS/PPS）→ 写不出 `avcC`。
- **`width`/`height` 必须是偶数**（YUV420P），openh264 会拒绝奇数尺寸。
- **`av_frame_make_writable` 每帧都要调**，因为编码器可能持有 frame 引用，直接覆盖会串帧。
- CapLite 需求里提到**流模式（fragmented）MP4 边录边落盘 + 结束后导出普通 MP4**。
  本次只用 `mp4` muxer 一次性写完；如果要做 fragmented，
  需要在 `avformat_write_header` 前把 `AVFormatContext.flags` 加上
  `AVFMT_FLAG_CUSTOM_IO`/`movflags`（走 `AVDictionary` 传 `movflags=+frag_keyframe+empty_moov`）。
  **本 spike 未实测 fragmented 输出，标注「未验证」。**

---

## 9. 明确标注「未验证」的部分

- 方案 **(c) `libloading` 运行时加载**：没写代码、没跑。
- 方案 **(d) C shim**（`build.rs` 调 gcc/cl.exe）：没跑。
- **独立 `openh264.dll` + 自写/Bento4 封装 MP4** 这条绕开 FFmpeg 的路线：只验证了
  DLL 的导出表（§2.4），**端到端没跑**。
- **fragmented MP4（流模式）输出**：没跑。
- **解码器之外的解码能力**（音频、其他容器）：没测。
- **多线程 / 大于 1 个 slice**、**非 640×360 分辨率**、**BGRA 之外的输入格式**：没测。
- **`libwinpthread-1.dll` 从 `C:\mingw64\bin` 失败的精确根因**（我只做到
  「实测 0xC0000139 + 确认两份 DLL 的 CRT 依赖不同：msvcrt vs UCRT」，
  没有去逐符号 diff 到底缺哪个入口点）。
- `D:\CapLite\includes\Bento4`、`libyuv` 的可用性：没测。
- 这台机器上是否**绝对**没有 libclang：我只搜了 §3 列出的那些根目录（未做全盘 `C:\` 扫描），
  但已覆盖 VS / Python / Qt / mingw / msys64 / .cargo / .rustup / Program Files / ProgramData。

---

## 10. 文件清单（全部在 `CanLapse\spikes\encode-mp4\` 内）

| 文件 | 说明 |
|---|---|
| `Cargo.toml` | 零依赖；`[workspace]` 隔离 |
| `build.rs` | 发 `rustc-link-search` / `rustc-link-lib`；支持 `FFMPEG_MIN_DIR` |
| `src/main.rs` | spike 主程序：枚举 codec → 编码 48 帧 → 解析 MP4 → 真实解码回读 → 断言 |
| `src/ffi.rs` | 手写 `extern "C"` + **实测偏移表** + 带类型读写访问器 |
| `src/mp4.rs` | 自写 ISO-BMFF box 解析器（`ftyp`/`moov`/`mdat`/`mvhd`/`mdhd`/`hdlr`/`stsd`/`avc1`/`avcC`/`stsz`/`stss`/`stts`/`stco`/`udta`/`meta`） |
| `run.cmd` | 一键复现（设 PATH → `cargo build --offline --release` → 运行） |
| `out.mp4` | **交付产物**，1,199,665 B，SHA256 `4059C635...D7A9CF` |
| `out_high.mp4` | `SPIKE_PROFILE=high` 的对照组 |
| `run.log` / `run_high.log` | 两次运行的完整输出（本文件所有引用的来源） |
| `build_verbose.log` | `cargo build -v`，含 rustc 链接参数 |
| `_probe/ffprobe_min.c` | **偏移量/枚举探针**（MSVC 编译，权威依据） |
| `_probe/build_probe.bat` | 探针构建脚本（内部调 `vcvars64.bat`） |
| `_probe/probe_out.txt` | 探针原始输出 |
| `_probe/bindgen-probe/` | 方案 (a) 失败证据 1：`bindgen` 默认 features 缺 `prettyplease` |
| `_probe/ffmpeg-sys-probe/` | 方案 (a) 失败证据 2/3/4：`out.txt` `out2.txt` `out3.txt`（最终 `Unable to find libclang`） |
