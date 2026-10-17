# 变更日志

本文件记录 CanLapse 的所有重要变更。

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### 新增

- **产品代码（垂直切片：拾取 → 差分 → PNG 缓存 → 合成 MP4）**
  - `src/capture.rs`：DWM 缩略图宿主 + `PrintWindow(PW_RENDERFULLCONTENT)` 抓帧，
    含目标尺寸变化时的自动重建、纯色画面告警兜底、以及诊断用的对照实现。
  - `src/frame.rs`：BGRA 帧与帧间差分（变化像素比例，供阈值判定），含 12 项单测。
  - `src/session.rs`：`{时间}_{会话名称}` 会话目录、`frame_xxxx.png`、`markers.json`
    （帧序号/时钟时间/耗时/备注 + 会话元信息），含历史会话列表与删除，含 2 项单测。
  - `src/png_io.rs`：PNG 读写。
  - `src/win.rs`：窗口枚举、进程名、鼠标拾取、消息泵、本地时间，含 5 项单测。
  - `src/main.rs`：命令行切片（`list` / `pick` / `record` / `sessions` / `encode` / `doctor` / `gui`）。
  - `src/encode.rs` + `src/encode/ffi.rs`：PNG 序列 → H.264 MP4（libopenh264）。
    FFI 偏移表**逐字节复制**自已验证的原型；生产版另加 RAII 释放、中途取消
    （取消后产出仍可播放）、混合尺寸帧自动统一缩放。
  - `build.rs`：链接自编译最小 ffmpeg，并把 5 个 `av*.dll` + `libwinpthread-1.dll`
    自动部署到 `target/<profile>` 与 `target/<profile>/deps`（后者供 `cargo test` 启动）。
- **单页 GUI（`src/ui/mod.rs`、`src/ui/floatbar.rs`，原生 Win32，无第三方 UI 框架）**
  - 460×455 单页：拾取按钮组 + 160×100 实时预览 + 状态提示、录制参数、导出参数、
    历史会话折叠面板、贴底整宽状态栏；底部操作区随 Idle / Recording / Encoding 切换。
  - 悬浮操作条 400×44：`TOPMOST | TOOLWINDOW | NOACTIVATE`，不抢绘画软件焦点；
    暂停 / 标记 / 停止、计时与帧数写在**可见控件**里（不是无标题栏窗口的标题）。
  - 历史会话列表（名称｜帧数）+ 重新导出 / 打开文件夹 / 删除；录制或合成中的会话禁止删除。
  - 两个弹窗：帮助（4 款绘画软件浮动画布指引）、大图预览（等比缩放、置前）。
  - 崩溃现场落盘：panic 写入 `%LOCALAPPDATA%\CanLapse\crash.log`（GUI 是 windows 子系统，
    stderr 看不见）。
- **技术验证原型**（保留作为依据，不属于产品代码）
  - `spikes/dwm-capture/`：DWM 抓帧可行性原型（6 种读回方式 × 可见/遮挡/最小化三态，
    跨进程 `--helper` 对端，按 tick 绘制确定性图案做逐像素自证）+ FINDINGS + 实测截图。
  - `spikes/encode-mp4/`：离线 H.264/MP4 编码原型（手写 FFI 直链 ffmpeg-min 的 libopenh264）
    + FINDINGS + `out.mp4`（48 帧 / 640×360 / 4.8s，经三重验证）。
- **运行日志（默认开启，含 exe 同目录镜像）**
  - `src/log.rs`：主日志 `%LOCALAPPDATA%\CanLapse\canlapse.log`，同时在 **exe 同目录**
    写一份 `canlapse.log` —— 用户跑的是 release 版、或 `%LOCALAPPDATA%` 不可写时也能拿到
    （实测：用户跑过一次却没生成日志，排查卡住）。`CANLAPSE_LOG` 指定主日志、
    `CANLAPSE_NOLOG=1` 关闭；超过 4MB 轮转为 `.old`。
  - 记录启动环境（虚拟桌面/主屏尺寸）、拾取全过程的窗口层级与点击结果、
    抓帧器/首帧结果、录制与合成结果、预览抓帧失败（10 秒限流）。
- **打包**
  - `tools/build.ps1`：一键构建（cargo build + 构建后资源注入）。
  - `tools/embed-resources.ps1` + `tools/ResInject.cs`：`.res` → `res.dll` → `UpdateResourceW`
    注入图标与版本信息；`.rc` 用 UTF-16LE 且必须 `#define VS_VERSION_INFO 1`（否则写出空壳）。

- `docs/requirements.md`：v1.0 需求基线；`docs/verification-log.md`：验证台账。
- ADR 0002（Rust + DWM 缩略图抓帧）、0003（手写 FFI 直链自编译最小 ffmpeg）、
  0004（UI 技术栈对比）、0005（Qt 类软件无画布子窗口）。

### 变更

- **性能（启动 / 抓图开销 / 崩溃兜底）**
  - 启动：会话列表改为**懒加载**（历史面板默认折叠，启动不再扫缓存目录里的
    每个会话目录与所有 PNG）；启动路径加计时日志。release 实测冷启动约 **31 ms**
    （6 次：31/31/32/31/32/31 ms），窗口创建 28 ms、首帧 45 ms（debug）。
  - 预览：抓帧改为 **DWM 端降采样**（输出上限宽 480），1920×986 目标每帧 7.4 MB →
    **472 KB**，单次抓帧 23 ms → 15 ms；主窗口最小化/不可见时**暂停预览并把缩略图
    `fVisible=0`**（实测 CPU 0.13% → 0%，DWM 停止合成）。
  - 录制：抓帧缓冲**复用**（两块缓冲交替，保存后交换），不再每帧分配/释放
    7.5 MB（1080p）/ 33 MB（4K）；每 40 个 tick 记一条 `[perf]` 抓帧耗时。
  - 防崩溃：所有窗口过程（主窗口 / 预览 / 悬浮条 / 抓帧宿主 / 拾取覆盖层 / 提示条）
    套 `catch_unwind` —— panic 不再跨越 `extern "system"` 边界把进程 abort；
    录制线程与合成线程同样接住 panic 并转成错误状态；新增**未处理异常过滤器**，
    把异常码与地址写进 `crash.log`。

- **分辨率统一（按 `add/分辨率统一.md` 的规格实现）**：录制以**第一张保存的画面**确定基准分辨率，
  之后所有帧**等比缩放 + 居中 + 黑边填充**到该尺寸再落盘；
  **差分始终用原始捕获帧**（填充的黑边会干扰差分、放大误触发）。
  实测：录制中途把目标窗口 480×320 改成 640×400，32 帧全部为 480×320，
  改后帧内容为 480×300 居中、上下各 10 行黑边。
- 新增 `Frame::get` / `resize_to` / `fit_into`：缩小用**面积平均**（避免降采样锯齿）、放大用双线性；
  未引入 `image` crate（该路径只在"录制中途改了窗口尺寸"时才会走到）。
- 录制信息栏实时显示基准分辨率，并在录制开始时给出"第一张画面决定视频分辨率"的提示。
- 技术栈由「待定」定为 **Rust**（`x86_64-pc-windows-msvc` + `windows-sys`，无第三方 UI 框架）。
- 与规格的两处有意偏差：窗口实际 460×455（内容需要更多高度，经确认观感次要）；
  历史会话用「列表 + 三个按钮」而不是"每行三个按钮"（460px 宽塞不下）。
- 按用户要求：拾取期间**最小化主窗口**（放弃半透明/鼠标穿透方案），并过滤输入法等系统浮层；
  抓帧失败不再弹模态框，改写状态栏供用户直接重试。
- 默认导出帧率 24fps；导出参数面板默认展开；录制按钮移到预览区右侧并变绿。
- 拾取诊断日志改为**显式开启**（设 `CANLAPSE_LOG=<路径>` 才写文件），默认不再往
  `%LOCALAPPDATA%\CanLapse\pick.log` 留调试残留。

### 修复

- 录制循环原本用一次 `sleep` 等完整采样间隔，低帧密度（如 1fps）下用户**短按 Esc/F8 会被整个漏掉**；
  改为把等待切成 20ms 小片轮询按键。
- 实跑才暴露的 GUI 缺陷（提醒：不要只靠读代码验收）：
  悬浮条时长写在无标题栏窗口的标题里、exe 控制台子系统弹黑框、
  `CanLapse.exe` 与 `canlapse.exe` 大小写同名互相覆盖、
  重新导出的产物被自动清理删掉、大图预览创建在绘画软件后面、录后台窗口一直暂停（补 `--no-fg-pause`）。
- 滑块"跳回、只能往左"：把 `TBM_GETPOS` 常量写成了 `TBM_SETRANGEMAX`。
- 拾取"点了半天没反应"：点击被自家置顶窗口吃掉；系统浮层（`Windows.UI.Core.CoreWindow`）
  把真正的候选顶掉；抓帧失败弹模态框挡住流程。
- 版本信息读出全空：`.rc` 漏了 `#define VS_VERSION_INFO 1`，写出的 `RT_VERSION` 是空壳
  （有名字项、无语言子项、0 字节）；另 `.rc` 必须存成 UTF-16LE，UTF-8 + `#pragma code_page`
  会让 `rc.exe` 静默产出近乎空的 `.res`。
- 预览区细节：占位文字用了窗口 DC 的默认 System 位图字体（巨大且被裁）、
  `DT_CALCRECT` 会收窄矩形导致"居中"变成贴左；抓帧失败后旧预览仍留在画面上。
- 拾取结束只调 `InvalidateRect`，而主窗口刚从小化恢复、WM_PAINT 时机不保证 →
  改为 `InvalidateRect` + `UpdateWindow` 同步重绘；预览的尺寸防御检查也不再
  在异常分支 `return`（那会跳过 `EndPaint`，每帧漏一个 DC）。
- **拾取把画布子窗口换成了整个主窗口**：`do_pick` 里一句"统一解析到顶层窗口"
  （`root_of`）把用户点中的画布子窗口替换成主窗口，预览里于是出现工具栏/颜色面板
  —— 用户看到的就是"没锁到画布"（SAI2 上实测；画布 0x100F0A 的裁剪应为 652×773）。
  改为保留用户点中的窗口，子窗口由抓帧器自己解析成「顶层缩略图 + rcSource 裁剪」。
- **抓帧宿主出现在任务栏预览里**：宿主窗口没加 `WS_EX_TOOLWINDOW`，鼠标停在任务栏
  图标上会看到一堆"CanLapse 抓帧宿主"；补上 `TOOLWINDOW | NOACTIVATE`。
- **拾取点击会画到画布上 / 点击收不到**：先试过 `WH_MOUSE_LL` 低级钩子（吞点击），
  但实测本程序里钩子安装成功、点击却一条都收不到（日志零条 `[hook]`，`GetAsyncKeyState`
  也读不到按下）。改为**全屏透明覆盖层**：拾取时铺一张覆盖虚拟桌面的窗口，点击必然落在
  它身上 → 记录坐标 → 隐藏自己 → `WindowFromPoint` 取该点下的真实窗口。
  目标软件一次点击都收不到（不会画点），也不依赖钩子/按键轮询；右键或 Esc 取消。
- **依赖目录被挪走后构建直接失败**（`D:\CapLite` → `D:\ffmpeg各版本\CapLite_c`，
  `link.exe` 报 LNK1181 找不到 `avformat.lib`）：`build.rs` 与 `doctor` 改为按
  「环境变量 → `tools\ffmpeg-min.path` → 本机已知位置」解析，两边顺序保持一致。

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
- **GUI 全流程**：460×455 主窗口、悬浮条三个按钮、进度条与取消合成、历史会话的
  重新导出/删除、大图预览与帮助弹窗均逐项点过（细节见 `docs/verification-log.md`）。
- **真实软件**：SAI2 画布子窗口、Krita（顶层窗口路径）、CSP 5.0.4 画布子窗口均实测；
  CSP 上录到 90 秒真实绘画过程 → 36 帧 → 3.000 秒成片（36÷12fps 精确吻合）。
- **拾取回归（四款软件）**：SAI2 / CSP / Photoshop / Krita **均能迅速锁定画布窗口**
  （用户 2026-10-17 实测；在此之前"锁不到画布"的链路已按本文档"修复"一节逐条修掉）。
- **4K 画布**：3840×2160（裸 `WS_POPUP`，绕开 WinForms 的 `WM_GETMINMAXINFO` 限制）产品路径实测：
  1fps×8s 录到 6 帧、基准 3840×2160、编码 3 秒出 0.6 秒成片；debug 构建峰值工作集 169.9 MB。
  已知边界：桌面之外的部分是黑的（屏幕外区域源窗口从未绘制，缩略图拿不到内容）。
- **SAI2 画布子窗口**：以 `--hwnd 0x100F0A`（"新建画布1 (42%)"）启动，抓帧器输出
  `crop=(698,79,652,773)`、预览只含画布内容（无工具栏/颜色面板）；
  抓帧宿主扩展样式实测 `0x08000080` = `TOOLWINDOW | NOACTIVATE`，不再出现在任务栏预览。
- **打包**：`tools\build.ps1` 产出的两个 exe 都能读出 `1.0.0.0` 版本信息、带自定义图标；
  GUI 入口无 `ConsoleWindowClass`。

### 尚未完成

- Photoshop 真机（本机未安装）；fragmented MP4。
- 桌面之外的区域抓不到（见"4K 画布"一条）—— 属 DWM 缩略图的固有边界，暂不处理。
- H.264 版税责任（自编译 libopenh264）—— 发布前必须处理，见 ADR 0003。

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
