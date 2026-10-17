# CanLapse

> 面向手绘创作者（SAI2 > Photoshop > CLIP STUDIO PAINT > Krita）的 **Windows 画布窗口缩时录屏工具**。

当前状态：**v1.0 功能完整、四款软件实测通过** —— 拾取窗口 → 差分 → 帧缓存（JPEG/PNG）→ 合成 MP4 全链路可用；
单页 GUI（460×455）、悬浮操作条、历史会话面板与两个弹窗均已完成。
SAI2 / CLIP STUDIO PAINT / Photoshop / Krita 均实测可迅速锁定画布窗口，
明细见 [docs/verification-log.md](docs/verification-log.md)。

> 📌 **先读这个**：[项目总结.md](项目总结.md) —— 一页纸讲清现状、架构、实测数据、坑与后续计划。

## 它怎么工作

1. 用户用鼠标**拾取画布窗口**（HWND），不做自动识别；
2. 通过 **DWM 窗口缩略图**持续取该窗口的画面；画面差分后按需把帧存进会话缓存目录（有内容存 JPEG q85，近空白存 PNG）
   —— 静止画面不存帧，这就是"缩时"的来源；
3. 录制结束后用**自编译的最小 OpenH264 编码器**批量合成 MP4；
4. 录制帧密度四档（1 帧/5 秒、1 帧/2 秒、1 帧/秒、2 帧/秒，默认 1 帧/5 秒）；导出固定 **30fps、画质中**。

设计约束（v1.0）：**单页面小巧主窗口（规格 460×380，实际 460×455，高度不够会压掉内容）**，
无独立设置面板；录制时主窗口最小化，只保留悬浮操作条；**无自动画布识别**。
完整需求见 [docs/requirements.md](docs/requirements.md)。

## 当前进度

| 模块 | 状态 | 证据 |
|---|---|---|
| 抓帧（DWM 缩略图宿主 + `PrintWindow`） | ✅ 端到端跑通 | [FINDINGS](spikes/dwm-capture/FINDINGS.md) |
| 帧差分（静止画面不存帧） | ✅ 已实测 | 4fps×6s：变化目标存 24 帧，静止目标存 1 帧 |
| 分辨率统一（等比+居中+黑边） | ✅ 已实测 | 录制中途 480×320→640×400，32 帧全为 480×320，后段 480×300 居中+上下各 10 行黑边 |
| 会话缓存 `frame_xxxx.jpg` / `.png` + `markers.json` | ✅ 有单测覆盖 | `cargo test`（29 项全过） |
| 录制容错（目标窗口消失 / 单帧失败跳过） | ✅ 已实测 | 目标关闭后自动停止并写 `completed:false` |
| 视频合成（H.264 MP4） | ✅ 已接入并端到端实测 | 24 帧 → 480×320 / 2.4s，`ffprobe -count_frames` = 24 |
| 取消合成（部分文件仍可播放） | ✅ 已实测 | `--frames 8` → 8 帧 / 0.8s，解码退出码 0 |
| 单页 GUI + 悬浮操作条 | ✅ 全流程实测 | 460×455；暂停/标记/停止、进度条、取消合成逐项点过 |
| 历史会话面板（重新导出/删除） | ✅ 已实测 | 名称+帧数列表；重新导出产出 `<名称>_<帧率>fps.mp4` |
| 帮助弹窗 / 预览大图 | ✅ 已实测 | 大图弹窗修正"创建在绘画软件后面" |
| 打包（图标 / 版本信息 / 清单） | ✅ 已实测 | `tools\build.ps1` 构建后注入；版本信息读出 `1.0.0.0` |

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
| UI | 原生 Win32（对比 egui 后选定；GUI 进程 25 MB vs 175 MB） | [ADR 0004](docs/decisions/0004-ui-stack-comparison.md) |
| Qt 类软件 | 无画布子窗口（alien widgets）→ 只能录整个窗口，建议 Canvas-only 模式 | [ADR 0005](docs/decisions/0005-qt-apps-no-canvas-child.md) |

抓帧这一项是产品的技术核心，已用原型严格实测（跨进程 + 遮挡 + 最小化三态，逐像素自证）：

* **可用**：宿主窗口 + `PrintWindow(宿主, PW_RENDERFULLCONTENT)` ——
  目标**可见 / 被完全遮挡 / 已最小化**三种状态下都能取到正确画面；
* **不可用**：`BitBlt` 读缩略图（恒为宿主自绘底色）；屏幕抓取（被遮挡即失效）；
  直接 `PrintWindow` 源窗口（在 Chromium 上直接返回失败，应用相关性太强）；
* 额外收益：抓到的**只有目标窗口自身的内容**，压在它上面的其他窗口不会进入画面。

## 用法

```powershell
# 推荐：一键构建（cargo build + 构建后注入图标/版本信息）
pwsh -File tools\build.ps1                  # debug
pwsh -File tools\build.ps1 -Profile release # release

# ⚠️ 直接 cargo build 不会注入图标与版本信息（资源是构建后注入的，见下文）
cargo build --offline

.\target\debug\CanLapse.exe                                       # 图形界面（双击这个；无控制台）
.\target\debug\canlapse-cli.exe list                              # 列出可见顶层窗口
.\target\debug\canlapse-cli.exe pick                              # 鼠标拾取画布窗口
.\target\debug\canlapse-cli.exe doctor                            # 自检：抓帧/编码/运行时 DLL/缓存
.\target\debug\canlapse-cli.exe record --pick --fps 2 --threshold 0.01 # 录制（Esc 停止，F8 打标记）
.\target\debug\canlapse-cli.exe record --title SAI --duration 30 --name 下雨天
.\target\debug\canlapse-cli.exe sessions                          # 历史缓存会话
.\target\debug\canlapse-cli.exe encode --session "<会话目录>" --fps 10 --quality high
cargo test --offline                                              # 单元测试
```

缓存默认在 `%LOCALAPPDATA%\CanLapse\cache`，可用 `--out` 或环境变量 `CANLAPSE_CACHE` 覆盖。

**构建依赖**：自编译最小 ffmpeg 的位置按以下顺序解析 ——
`CANLAPSE_FFMPEG_MIN` → `tools\ffmpeg-min.path`（一行路径）→ 本机已知位置
（`D:\CapLite\ffmpeg-min`、`D:\ffmpeg各版本\CapLite_c\ffmpeg-min` 等）。
本机 `D:\CapLite` 被整体挪到 `D:\ffmpeg各版本\CapLite_c` 后，`build.rs` 与 `doctor` 都会自动找到。

**排查日志**：默认开启 —— 主日志 `%LOCALAPPDATA%\CanLapse\canlapse.log`，
并在 **exe 同目录**留一份 `canlapse.log` 镜像（沙箱/只读环境下也能拿到）。
`CANLAPSE_LOG=<路径>` 可指定主日志，`CANLAPSE_NOLOG=1` 关闭；超过 4MB 自动轮转为 `.old`。
拾取过程会记录光标经过的窗口层级、点击结果、抓帧器输出尺寸与失败原因 ——
遇到"点不上/预览不对"直接把日志发出来即可定位。

## 目录结构

```
CanLapse/
├─ src/                 产品代码（抓帧 / 差分 / 会话 / 图像编解码 / 编码 / 原生 Win32 GUI）
├─ tools/               构建与打包脚本（build.ps1 一键构建；embed-resources.ps1 资源注入）
├─ docs/
│  ├─ requirements.md   v1.0 需求基线
│  ├─ verification-log.md 验证台账（只记跑出来的事实）
│  └─ decisions/        架构决策记录（ADR 0001~0005）
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
3. **超大画布**：产品路径实测 3840×2160 可抓可编码，但**桌面之外的部分是黑的**
   —— 窗口比桌面大时，屏幕外区域源窗口从未绘制，DWM 缩略图拿不到内容。
   这是"缩略图 = 屏幕合成结果"的固有边界。
4. **依赖实现行为**：`缩略图 + PW_RENDERFULLCONTENT` 没有微软的明确文档承诺，
   代码里已有纯色画面告警作为兜底。
5. **目标应用差异**：SAI2 / CSP / Photoshop / Krita 均已实测可拾取（Qt 类软件无画布子窗口，
   只能录整个窗口，见 ADR 0005；建议按 `Tab` 进 Canvas-only 模式）。

## 许可

[MIT](LICENSE)。注意第三方组件的许可与版税义务见上方"已知风险"。
