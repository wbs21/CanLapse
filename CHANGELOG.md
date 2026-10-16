# 变更日志

本文件记录 CanLapse 的所有重要变更。

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### 新增

- **产品代码（垂直切片的抓帧半边）**
  - `src/capture.rs`：DWM 缩略图宿主 + `PrintWindow(PW_RENDERFULLCONTENT)` 抓帧，
    含目标尺寸变化时的自动重建、纯色画面告警兜底、以及诊断用的对照实现。
  - `src/frame.rs`：BGRA 帧与帧间差分（变化像素比例，供阈值判定），含 6 项单测。
  - `src/session.rs`：`{时间}_{会话名称}` 会话目录、`frame_xxxx.png`、`markers.json`
    （帧序号/时钟时间/耗时/备注 + 会话元信息），含历史会话列表与删除，含 2 项单测。
  - `src/png_io.rs`：PNG 读写。
  - `src/win.rs`：窗口枚举、进程名、鼠标拾取、消息泵、本地时间，含 3 项单测。
  - `src/main.rs`：命令行切片（`list` / `pick` / `record` / `sessions` / `encode`）。
- **技术验证原型**
  - `spikes/dwm-capture/`：DWM 抓帧可行性原型（6 种读回方式 × 可见/遮挡/最小化三态，
    跨进程 `--helper` 对端，按 tick 绘制确定性图案做逐像素自证）+ FINDINGS + 实测截图。
  - `spikes/encode-mp4/`：离线 H.264/MP4 编码原型（手写 FFI 直链 ffmpeg-min 的 libopenh264）
    + FINDINGS + `out.mp4`（48 帧 / 640×360 / 4.8s，经三重验证）。
- `docs/requirements.md`：v1.0 需求基线。
- ADR 0002（Rust + DWM 缩略图抓帧）、ADR 0003（手写 FFI 直链自编译最小 ffmpeg 编码）。

### 变更

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

### 尚未完成

- `src/encode.rs` 仍是明确返回 `Err` 的占位实现，**视频合成尚未接入正式代码**。
- 单页 GUI（460×380）、悬浮操作条、历史会话面板 UI、两个弹窗均未开始。
- 超大画布（4K）、SAI2/Photoshop/CSP 真机、fragmented MP4 未验证。

### 备注（对原始需求的两处纠正）

1. `ffmpeg-min` 的 `avcodec-63.dll` 里 **`h264` 解码器并没有被裁掉**，
   因此编码验证可以做真实解码回读。
2. 「自编译最小方案」的出处不在 `D:\CapLite` 下，而在 `D:\tools\`
   （`build-oh264.sh` + `wrappers/pkgconf-fit.c`），openh264 2.6.0 是**静态编进 `avcodec-63.dll`** 的。

[Unreleased]: https://example.com/canlapse/compare/v0.0.0...HEAD
