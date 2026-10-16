# 0003 — 视频编码：手写 FFI 直链「自编译最小 ffmpeg（libopenh264）」

- 状态：已接受，**已接入 `src/encode.rs` 并端到端实测出片**
- 日期：2026-02-14
- 关联证据：[spikes/encode-mp4/FINDINGS.md](../../spikes/encode-mp4/FINDINGS.md)（含完整复现步骤与实测偏移表）

## 背景

需求：录制结束后用**自编译最小 OpenH264** 批量合成 MP4，导出参数（播放帧率/画质/输出路径）
独立于录制参数。

环境约束：**离线**（crates.io 不通，只能用本地 registry 缓存里的 crate）、
target 为 `x86_64-pc-windows-msvc`、预编译 ffmpeg 位于 `D:\CapLite\ffmpeg-min`。

## 决定

采用**手写 `extern "C"` 声明 + 直接链 ffmpeg-min 的 MSVC 导入库**，编码 crate 零第三方依赖：

```toml
[dependencies]   # 故意为空：--offline 完全不碰 registry
```

`build.rs` 输出的链接参数：

```
-L native=D:\CapLite\ffmpeg-min\bin -l dylib=avformat -l dylib=avcodec -l dylib=swscale -l dylib=avutil
```

`ffmpeg-min\bin\*.lib` 是原生 x64 COFF 导入库，MSVC `link.exe` 直接可用，**不需要 MinGW**。
结构体只声明真正用到的字段，**偏移量用本机 `cl.exe` + 同一套头文件实测 `offsetof`**
（探针保留在 `spikes/encode-mp4/_probe/ffprobe_min.c`）。

## 依据（父代理已独立复核）

`out.mp4`：1,199,665 字节，SHA256 `4059C635…D7A9CF`，H.264 `avc1` 640×360，48 帧，4.800 s @10fps。

1. 自写 MP4 box 解析：`ftyp/free/mdat/moov` 齐备，1 条 `vide` 轨，`stsz` = 48 samples，`stss` = 5 关键帧；
2. **真实解码回读**：ffmpeg-min 自带的 `mov` demuxer + `h264` 解码器解回 48 帧 640×360，
   逐帧亮度与源 BGRA 帧 **Pearson r = 0.99992**；
3. 完整构建的 `ffprobe -count_frames` → `nb_read_frames=48`、`duration=4.800000`、
   `bit_rate≈1999441`；`ffmpeg -f null -` 退出码 0。

实测**失败**的替代方案：`ffmpeg-sys-next` + `ffmpeg-next`（需要 bindgen → 需要 libclang）。
本机确实没有 `libclang.dll`（VS / Python / Qt / mingw64 / msys64 / .cargo / .rustup 全查过，0 命中），
并且其默认 features 还缺缓存里的 `prettyplease`。三段失败证据见 spike 的 FINDINGS.md。

## 两处对原有认知的纠正

1. **`h264` 解码器并没有被裁掉**。原始需求写的是"只保留编码，去除解码"，
   但实测 `D:\CapLite\ffmpeg-min\bin\avcodec-63.dll` 里同时含 `h264` 编解码器，
   所以解码回读验证是可以真做的（本次也确实做了）。
2. **「自编译最小方案」的出处不在 `D:\CapLite` 下，而在 `D:\tools\`**：
   `build-oh264.sh`（openh264 2.6.0 → `oh264-static\lib\libopenh264.a`）、
   `oh264-src\`（源码树），以及关键的 `wrappers\pkgconf-fit.c`
   （22 行 C，硬编码 `--with-path=D:\tools\oh264-static\lib\pkgconfig`，
   专门骗过 FFmpeg configure 的 pkg-config 探针以链**静态** openh264）。
   `libopenh264` 是这套构建里**唯一**的视频编码器，**静态编进了 `avcodec-63.dll`**，
   不依赖 `D:\CapLite\includes\openh264\openh264.dll`（那份是独立构建，未被 avcodec 使用）。

## 后果 / 必须遵守（都是实测踩出来的）

1. **DLL 部署**：`avcodec-63.dll` 依赖 `libwinpthread-1.dll`，而 `ffmpeg-min\bin\` 里**没有**。
   必须随包分发，且只能用 `D:\CapLite\build` 或 msys64 那份 ——
   用 `C:\mingw64\bin` 那份会 **0xC0000139 STATUS_ENTRYPOINT_NOT_FOUND**（UCRT 系 vs msvcrt 系不匹配）；
   完全没有则 **0xC0000135**。
2. **`avformat_write_header` 会改写 `st->time_base`**（请求 1/1000 → mov muxer 实际选 1/16000）。
   必须在**写头之后**再读它做 `av_packet_rescale_ts`，否则时长差 16 倍。
3. **`AVFormatContext.streams` 是 `AVStream **`**（指向指针数组），不是内联数组；
   当内联数组解再解引用会直接 `0xC0000005`。
4. **`width/height/pix_fmt/framerate/gop_size/max_b_frames` 不是 AVOptions**
   （设了会返回 `AVERROR_OPTION_NOT_FOUND`），只能走结构体偏移；
   `AV_CODEC_FLAG_GLOBAL_HEADER` 必须在 `avcodec_open2` **之前**设，否则 mp4 拿不到 `avcC`。
   宽高必须是偶数，每帧要 `av_frame_make_writable`。
5. **偏移表随版本失效**：升级 `ffmpeg-min` 必须重跑探针并更新 `src/ffi.rs` 的偏移模块。
   因此探针源码与偏移表要一起入库。
6. **画质档位建议用 high**：默认的 `constrained_baseline` 会触发一条无害但吵的
   openh264 警告（`doesn't support profile(578)`）；`high` 实测体积小 6.5%、质量更好且无警告。
7. **⚠️ 授权（发布前必须处理）**：openh264 源码是 Cisco BSD-2 条款，但
   **Cisco 代付 MPEG-LA 版税的承诺只覆盖"从 Cisco 官方渠道下载的二进制"**；
   本项目用的是**自己编译的静态库**，不在代付范围内 →
   **H.264 版税责任落在发布方**。FFmpeg 侧未开 `--enable-gpl/--enable-nonfree`，按 LGPL v2.1+ 走。

## 尚未验证

1. **fragmented（流模式）MP4**（`movflags=+frag_keyframe+empty_moov`）未实测。
2. **`libloading` 运行时加载**路线未实测。它在本地缓存中（`libloading-0.8.9`），
   且与上一代产品 CapLite「FFmpeg 仅运行时 LoadLibrary、不静态链接以保持 LGPL 合规」的做法一致，
   是缺库降级与许可隔离的备选路线。
3. **完全绕开 FFmpeg**（独立 `openh264.dll` + Bento4 `ap4.lib` 或自写 MP4 封装）未实测。
4. **正式 `src/encode.rs` 尚未接入**：目前是**明确返回 Err 的占位实现**，不会静默产出坏文件。

## 与 0002 的关系

0002 决定"谁来抓帧"，0003 决定"谁来编码"，两者互不依赖：
抓帧走 DWM 缩略图，编码走 ffmpeg-min 的 libopenh264。导出参数与录制参数彼此独立，
由上层（历史会话的「重新导出」）组合。

## 接入实现（`src/encode.rs`）

原型（spike）只关心"能不能跑通"，生产代码额外做了三件事：

1. **FFI 对象 RAII 释放**：`Muxer` 持有 `oc/ctx/frame/pkt/sws`，任何一步失败直接返回错误，
   由 `Drop` 统一释放；不会像原型那样在早退路径上泄漏 context。
2. **合成可中途取消**（导出面板的【取消合成】）：`progress` 回调返回 `Progress::Cancel` 后
   仍然 `avcodec_send_frame(NULL)` 冲刷并写 trailer，因此**已编码的部分是可播放的视频**，
   而不是一个损坏文件。实测：`--frames 8` → 8 帧 / 0.800s，`ffmpeg -f null -` 退出码 0。
3. **混合尺寸会话**：录制过程中用户若改了画布窗口大小，同一会话里会有不同尺寸的帧；
   编码时统一缩放到**首帧**尺寸（宽高向下取偶数，libopenh264 要求偶数），而不是直接失败。

`build.rs` 负责链接参数与 **运行时 DLL 部署**：把 5 个 `av*.dll` 与 `libwinpthread-1.dll`
复制到 `target/<profile>` 与 `target/<profile>/deps`（后者是为了让 `cargo test` 的测试
可执行文件也能启动——加载器只在 exe 自己的目录里找 DLL）。

### 接入后实测（端到端）

录制（DWM 抓帧，4fps×6s）→ 24 张 PNG → 合成：

```
codec_name=h264  profile=High  width=480  height=320  pix_fmt=yuv420p
r_frame_rate=10/1  nb_read_frames=24  duration=2.400000   （ffprobe -count_frames）
ffmpeg -f null - 退出码 0
```

从产出的 MP4 解出第 0 帧与源 `frame_0000.png` 目视比对：位条图案、四象限颜色、
移动方块位置完全一致（仅边缘有 4:2:0 色度下采样带来的轻微过渡）。

### 决定性证据：这套 ffmpeg 的真实构建参数

`canlapse doctor` 会打印 `avcodec_configuration()`，实测输出：

```
--prefix='D:/tools/ffmpeg-min-new' --arch=x86_64 --target-os=mingw32
--enable-shared --disable-static --disable-programs --disable-doc --disable-debug
--disable-autodetect --disable-network --enable-libopenh264
--pkg-config='D:/tools/wrappers/pkgconf-fit.exe'
--disable-everything --enable-encoder='libopenh264,aac' --enable-decoder='h264,aac'
--enable-parser='h264,aac' --enable-muxer='mp4,mov'
--enable-demuxer='mov,mp4,matroska' --enable-protocol=file
--enable-swscale --enable-swresample
--extra-ldflags='-static-libstdc++ -static-libgcc' --extra-version=-capmin
```

据此可以确证三件事：

* **视频编码器只有 `libopenh264` 一个，没有 x264** —— 完全符合需求；
* 解码头 `h264,aac` 确实在（与"只保留编码、去除解码"的原始描述不符，见上文纠正）；
* 构建出处是 `D:/tools/ffmpeg-min-new`，用的正是那个 `D:/tools/wrappers/pkgconf-fit.exe`
  包装器；且 `--disable-network`，本地工具没有网络面。

