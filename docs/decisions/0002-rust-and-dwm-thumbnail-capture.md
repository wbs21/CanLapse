# 0002 — 采用 Rust，并以 DWM 缩略图宿主窗口作为抓帧核心

- 状态：已接受
- 日期：2026-02-14
- 关联证据：[spikes/dwm-capture/FINDINGS.md](../../spikes/dwm-capture/FINDINGS.md)

## 背景

CanLapse 要抓的是**别的进程里的画布窗口**（SAI2 / Photoshop / CLIP STUDIO PAINT / Krita），
并且要求：用户把画布用其他窗口盖住时（比如旁边开着参考图、聊天窗口）**仍然能继续录制**。

原始方案写的是"DWM 窗口缩略图捕获指定 HWND"。但 `DwmRegisterThumbnail` 只负责把源窗口的实时
缩略图**合成**到目标窗口上，Win32 并没有提供"取缩略图像素"的 API。若不实测，
很可能写完捕获模块才发现拿不到像素。因此在写正式代码前先做了可行性原型
（`spikes/dwm-capture/`）。

同时，技术栈由最初的 C++20 改为 **Rust**（用户决定）。

## 决定

1. **技术栈：Rust**（`rustc 1.99`，`x86_64-pc-windows-msvc`）。
   Win32 API 通过 `windows-sys` 调用，不引入第三方 UI 框架。
2. **抓帧核心：DWM 缩略图宿主窗口 + `PrintWindow(host, PW_RENDERFULLCONTENT)`**。
   具体为：
   - 建一个宿主顶层窗口，`DwmRegisterThumbnail(host, canvas_hwnd)`；
   - `DwmUpdateThumbnailProperties` 设 `DWM_TNP_RECTDESTINATION | RECTSOURCE | OPACITY | VISIBLE | SOURCECLIENTAREAONLY`，
     源矩形取源窗口**客户区**，目标矩形取宿主客户区，两者尺寸相同以保证 **1:1 不缩放**；
   - 抓帧时对**宿主**调用 `PrintWindow(host, hdc, PW_RENDERFULLCONTENT)`。
3. **不采用**以下方案作为主路径：
   - 屏幕 `BitBlt`：目标被遮挡即失效，最小化时客户区退化为 0×0；
   - 对源窗口直接 `PrintWindow`：应用相关性太强，在 Chromium 上直接失败；
   - `BitBlt` 宿主窗口 DC：实测恒定为宿主自绘底色，永远读不到缩略图。

## 依据（均为本机实测，详见 FINDINGS.md）

| 方式 | 目标可见 | 被完全遮挡 | 源已最小化 |
|---|---|---|---|
| 屏幕 BitBlt | ✅ | ❌ | ❌ |
| `PrintWindow(源, PW_RENDERFULLCONTENT)` | ✅ | ✅ | ❌ 全黑 |
| **宿主 + `PrintWindow(宿主, PW_RENDERFULLCONTENT)`** | ✅ | ✅ | ✅ |

* 自证方式：源窗口按 tick 画四象限纯色 + 16 位二进制计数条 + 移动方块，抓帧后逐像素核对；
  跨进程实验中对端进程把 tick 写进窗口标题，主进程读回作为预期值。
* Chromium（GPU 合成）上：直接 `PrintWindow` **返回失败(0)**，而宿主方式成功并拿到完整画面。
* 宿主移到屏幕外 4000px 仍有效 → 宿主**不需要显示在屏幕上**。
* Krita 崩溃对话框压在目标窗口上时，宿主方式抓到的画面**不含该对话框**，
  证明它取的是"该窗口自身的内容"而非屏幕像素。

## 后果

* 捕获与目标应用是否支持 `WM_PRINT` 无关，走的是 DWM 合成器那一层。
* 需要维护一个宿主窗口及其生命周期，并处理 `DwmRegisterThumbnail` 失败
  （桌面合成关闭、HWND 失效）的降级与告警。
* 该组合（缩略图 + `PW_RENDERFULLCONTENT`）**没有微软的明确文档承诺**，属于依赖实现行为。
  正式实现必须带自检：当抓到全黑或恒定画面时给出告警，而不是静默产出坏录像。

## 尚未验证（见 FINDINGS.md 第八节）

超大画布（4K）、桌面合成关闭、Windows 10 早期版本 / 远程桌面、
SAI2 / Photoshop / CSP 真机、Krita 主窗口 + 浮动画布。
其中 **4K 画布下的宿主尺寸与 1:1 缩放**应在进入正式抓帧模块前优先补测。
