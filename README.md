# CanLapse

> 面向手绘创作者（SAI2 > Photoshop > CLIP STUDIO PAINT > Krita）的 **Windows 画布窗口缩时录屏工具**。

当前状态：**垂直切片已闭环** —— 拾取窗口 → 差分 → PNG 缓存 → 合成 MP4 已端到端实测出片；
GUI（460×380 单页 + 悬浮操作条）尚未开始。

## 它怎么工作

1. 用户用鼠标**拾取画布窗口**（HWND），不做自动识别；
2. 通过 **DWM 窗口缩略图**持续取该窗口的画面；画面差分后按需把 PNG 存进会话缓存目录
   —— 静止画面不存帧，这就是"缩时"的来源；
3. 录制结束后用**自编译的最小 OpenH264 编码器**批量合成 MP4；
4. 录制参数与导出参数**相互独立**（帧密度 1~8fps 决定录制，播放帧率 5/10/15/20/24fps 决定导出）。

设计约束（v1.0）：**单页面小巧主窗口 460×380，无独立设置面板**；录制时主窗口最小化，
只保留悬浮操作条；**无自动画布识别**。完整需求见 [docs/requirements.md](docs/requirements.md)。

## 当前进度

| 模块 | 状态 | 证据 |
|---|---|---|
| 抓帧（DWM 缩略图宿主 + `PrintWindow`） | ✅ 端到端跑通 | [FINDINGS](spikes/dwm-capture/FINDINGS.md) |
| 帧差分（静止画面不存帧） | ✅ 已实测 | 4fps×6s：变化目标存 24 帧，静止目标存 1 帧 |
| 分辨率统一（等比+居中+黑边） | ✅ 已实测 | 录制中途 480×320→640×400，32 帧全为 480×320，后段 480×300 居中+上下各 10 行黑边 |
| 会话缓存 `frame_xxxx.png` + `markers.json` | ✅ 有单测覆盖 | `cargo test`（11 项全过） |
| 录制容错（目标窗口消失 / 单帧失败跳过） | ✅ 已实测 | 目标关闭后自动停止并写 `completed:false` |
| 视频合成（H.264 MP4） | ✅ 已接入并端到端实测 | 24 帧 → 480×320 / 2.4s，`ffprobe -count_frames` = 24 |
| 取消合成（部分文件仍可播放） | ✅ 已实测 | `--frames 8` → 8 帧 / 0.8s，解码退出码 0 |
| 单页 GUI 460×380 + 悬浮操作条 | ❌ 未开始 | — |
| 历史会话面板（重新导出/打开/删除） | 🟡 数据层已有，UI 未做 | `canlapse sessions` |
| 帮助弹窗 / 预览大图 | ❌ 未开始 | — |

编码走**导入库静态链接**：`build.rs` 会按 `CANLAPSE_FFMPEG_MIN`（默认 `D:\CapLite\ffmpeg-min`）
输出链接参数，并把 6 个运行时 DLL（5 个 `av*` + `libwinpthread-1.dll`）自动复制到
`target/<profile>` 与 `target/<profile>/deps`。**DLL 必须与 exe 同目录**——导入表在进程启动时
就要解析，那时 `main()` 还没运行，在代码里 `SetDllDirectory` 来不及。

`src/encode.rs` 相对原型多做了三件生产代码才需要的事：FFI 对象 RAII 释放、
合成可中途取消（取消后仍写 trailer，产出**可播放的部分视频**）、
会话里混有不同尺寸帧时（录制中用户改了画布窗口大小）自动统一缩放。

## 关键决策

| 决策 | 内容 | 记录 |
|---|---|---|
| 技术栈 | Rust（`x86_64-pc-windows-msvc`）+ `windows-sys`，不用第三方 UI 框架 | [ADR 0002](docs/decisions/0002-rust-and-dwm-thumbnail-capture.md) |
| 抓帧 | **DWM 缩略图宿主 + `PrintWindow(宿主, PW_RENDERFULLCONTENT)`** | 同上 |
| 编码 | 手写 FFI 直链自编译最小 ffmpeg（libopenh264），编码 crate 零依赖 | [ADR 0003](docs/decisions/0003-video-encoding-ffmpeg-min-openh264.md) |

抓帧这一项是产品的技术核心，已用原型严格实测（跨进程 + 遮挡 + 最小化三态，逐像素自证）：

* **可用**：宿主窗口 + `PrintWindow(宿主, PW_RENDERFULLCONTENT)` ——
  目标**可见 / 被完全遮挡 / 已最小化**三种状态下都能取到正确画面；
* **不可用**：`BitBlt` 读缩略图（恒为宿主自绘底色）；屏幕抓取（被遮挡即失效）；
  直接 `PrintWindow` 源窗口（在 Chromium 上直接返回失败，应用相关性太强）；
* 额外收益：抓到的**只有目标窗口自身的内容**，压在它上面的其他窗口不会进入画面。

## 用法（命令行切片）

```powershell
cargo build --offline

.\target\debug\canlapse.exe list                                  # 列出可见顶层窗口
.\target\debug\canlapse.exe pick                                  # 鼠标拾取画布窗口
.\target\debug\canlapse.exe doctor                                # 自检：抓帧/编码/运行时 DLL/缓存
.\target\debug\canlapse.exe record --pick --fps 2 --threshold 0.01 # 录制（Esc 停止，F8 打标记）
.\target\debug\canlapse.exe record --title Krita --duration 30 --name 下雨天
.\target\debug\canlapse.exe sessions                              # 历史缓存会话
.\target\debug\canlapse.exe encode --session "<会话目录>" --fps 10 --quality high
cargo test --offline                                              # 单元测试
```

缓存默认在 `%LOCALAPPDATA%\CanLapse\cache`，可用 `--out` 或环境变量 `CANLAPSE_CACHE` 覆盖。

## 目录结构

```
CanLapse/
├─ src/                 产品代码（抓帧/差分/会话/PNG 已实现；encode 待接入；GUI 待做）
├─ tests/               预留
├─ docs/
│  ├─ requirements.md   v1.0 需求基线
│  └─ decisions/        架构决策记录（ADR 0001~0003）
├─ spikes/              技术验证原型（保留作为依据，不属于产品代码）
│  ├─ dwm-capture/      DWM 抓帧可行性原型 + FINDINGS.md + 实测截图
│  └─ encode-mp4/       离线 H.264/MP4 编码原型 + FINDINGS.md + out.mp4
└─ ...
```

## 已知风险

1. **⚠️ 授权**：libopenh264 是**自己编译的静态库**，
   Cisco 代付 MPEG-LA 版税的承诺只覆盖官方二进制下载 → **H.264 版税责任在发布方**。
   发布前必须处理。详见 [ADR 0003](docs/decisions/0003-video-encoding-ffmpeg-min-openh264.md)。
2. **DLL 部署**：`avcodec-63.dll` 依赖 `libwinpthread-1.dll`，而 `ffmpeg-min\bin` 里没有它；
   发错版本会以 `0xC0000139` 启动失败。
3. **超大画布未验证**：抓帧实测最大 1302×887；4K 画布下的宿主尺寸与是否 1:1 缩放需补测。
4. **依赖实现行为**：`缩略图 + PW_RENDERFULLCONTENT` 没有微软的明确文档承诺，
   代码里已有纯色画面告警作为兜底。
5. **目标应用差异**：SAI2 / Photoshop / CSP 本机未安装，尚未验证；
   Krita 因本机环境限制（其资源数据库写入被沙箱拦截，启动即崩）未能完成主窗口验证。

## 许可

[MIT](LICENSE)。注意第三方组件的许可与版税义务见上方"已知风险"。
