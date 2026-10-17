# 变更日志

本文件记录 CanLapse 的所有重要变更。

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### 新增

- **产品代码（垂直切片：拾取 → 差分 → PNG 缓存 → 合成 MP4）**
  - `src/capture.rs`：DWM 缩略图宿主 + `PrintWindow(PW_RENDERFULLCONTENT)` 抓帧，
    含目标尺寸变化时的自动重建、纯色画面告警兜底、以及诊断用的对照实现。
  - `src/frame.rs`：BGRA 帧与帧间差分（变化像素比例，供阈值判定），含 6 项单测。
  - `src/session.rs`：`{时间}_{会话名称}` 会话目录、`frame_xxxx.png`、`markers.json`
    （帧序号/时钟时间/耗时/备注 + 会话元信息），含历史会话列表与删除，含 2 项单测。
  - `src/png_io.rs`：PNG 读写。
  - `src/win.rs`：窗口枚举、进程名、鼠标拾取、消息泵、本地时间，含 3 项单测。
  - `src/main.rs`：命令行切片（`list` / `pick` / `record` / `sessions` / `encode` / `doctor`）。
  - `src/encode.rs` + `src/encode/ffi.rs`：PNG 序列 → H.264 MP4（libopenh264）。
    FFI 偏移表**逐字节复制**自已验证的原型；生产版另加 RAII 释放、中途取消
    （取消后产出仍可播放）、混合尺寸帧自动统一缩放。
  - `build.rs`：链接自编译最小 ffmpeg，并把 5 个 `av*.dll` + `libwinpthread-1.dll`
    自动部署到 `target/<profile>` 与 `target/<profile>/deps`（后者供 `cargo test` 启动）。
- **技术验证原型**
  - `spikes/dwm-capture/`：DWM 抓帧可行性原型（6 种读回方式 × 可见/遮挡/最小化三态，
    跨进程 `--helper` 对端，按 tick 绘制确定性图案做逐像素自证）+ FINDINGS + 实测截图。
  - `spikes/encode-mp4/`：离线 H.264/MP4 编码原型（手写 FFI 直链 ffmpeg-min 的 libopenh264）
    + FINDINGS + `out.mp4`（48 帧 / 640×360 / 4.8s，经三重验证）。
- `docs/requirements.md`：v1.0 需求基线。
- ADR 0002（Rust + DWM 缩略图抓帧）、ADR 0003（手写 FFI 直链自编译最小 ffmpeg 编码）。

### 变更

- **分辨率统一（按 `add/分辨率统一.md` 的规格实现）**：录制以**第一张保存的画面**确定基准分辨率，
  之后所有帧**等比缩放 + 居中 + 黑边填充**到该尺寸再落盘；
  **差分始终用原始捕获帧**（填充的黑边会干扰差分、放大误触发）。
  实测：录制中途把目标窗口 480×320 改成 640×400，32 帧全部为 480×320，
  改后帧内容为 480×300 居中、上下各 10 行黑边。
- 新增 `Frame::get` / `resize_to` / `fit_into`：缩小用**面积平均**（避免降采样锯齿）、放大用双线性；
  未引入 `image` crate（该路径只在"录制中途改了窗口尺寸"时才会走到）。
- 录制信息栏实时显示基准分辨率，并在录制开始时给出"第一张画面决定视频分辨率"的提示。
- 技术栈由「待定」定为 **Rust**（`x86_64-pc-windows-msvc` + `windows-sys`，无第三方 UI 框架）。
- 项目骨架填充为真实内容：README 改为产品说明与进度表。

### 修复

- 录制循环原本用一次 `sleep` 等完整个采样间隔，低帧密度（如 1fps）下用户**短按 Esc/F8 会被整个漏掉**；
  改为把等待切成 20ms 小片轮询按键。

### 已验证（实测证据）

- 抓帧：目标窗口可见 / 被完全遮挡 / 已最小化三态均能取到正确画面（跨进程同样成立）；
  压在目标上的其他窗口不会进入画面。
- 差分：4fps×6s 下，持续变化的目标存 24 帧，静止目标只存 1 帧。
- 容错：目标窗口被关闭后录制自动停止，元信息写入 `completed: false`、`duration_ms: 4500`。
- 编码：`out.mp4` 经自写 box 解析 + ffmpeg-min 真实解码回读（Pearson r=0.99992）
  + 完整构建 `ffprobe -count_frames`（`nb_read_frames=48`）三重确认。
- **端到端出片**：DWM 抓帧录制 24 帧 → 合成 MP4
  （H.264 **High** profile / 480×320 / yuv420p / 2.400s），`ffprobe -count_frames` = 24、
  `ffmpeg -f null -` 退出码 0；解出第 0 帧与源 `frame_0000.png` 目视一致。
- **取消合成**：`--frames 8`（走 GUI【取消合成】同一条代码路径）→ 8 帧 / 0.800s，
  解码退出码 0 —— 取消后的文件是**可播放的部分视频**，不是损坏文件。
- `canlapse doctor` 打印的 `avcodec_configuration()` 确证：这套构建的视频编码器
  **只有 libopenh264、没有 x264**（`--disable-everything --enable-encoder='libopenh264,aac'`）。

### 尚未完成

- 单页 GUI（460×380）、悬浮操作条、历史会话面板 UI、两个弹窗均未开始。
- 超大画布（4K）、SAI2/Photoshop/CSP 真机、fragmented MP4 未验证。

### 备注（对原始需求的两处纠正）

1. `ffmpeg-min` 的 `avcodec-63.dll` 里 **`h264` 解码器并没有被裁掉**，
   因此编码验证可以做真实解码回读。
2. 「自编译最小方案」的出处不在 `D:\CapLite` 下，而在 `D:\tools\`
   （`build-oh264.sh` + `wrappers/pkgconf-fit.c`），openh264 2.6.0 是**静态编进 `avcodec-63.dll`** 的。

### 备注（两条与 `add/` 参考材料不一致之处，需决策）

3. `add/openh264.md` 描述的是「仅编码器、`DISABLEDECODER=1`」的最小 openh264 构建，
   但当前实际链接的 ffmpeg-min 是**编解码都带**的：
   `--enable-encoder='libopenh264,aac' --enable-decoder='h264,aac'`（由 `canlapse doctor` 打印证实）。
   若确实要纯编码器版，需按该文档重新编译 openh264 并改为**直接调用 WelsEnc\***（绕开 libavcodec）。
4. `add/分辨率统一.md` 里的合成命令用 `-c:v libx264`，但这套 ffmpeg-min **没有 x264**
   （`--disable-everything --enable-encoder='libopenh264,aac'`），只能用 libopenh264。

[Unreleased]: https://example.com/canlapse/compare/v0.0.0...HEAD
