# 0004 — GUI 技术路线对比：egui/eframe vs 原生 Win32

- 状态：**待决策**（本文只提供实测数据与权衡，不代替决策）
- 日期：2026-02-14
- 触发：`CanLapse/add/` 下出现 egui 版 UI 草稿（`egui.rs`、`auto_clear.rs`、`button.md`、`render.md`）
- 证据：探针 `spikes/ui-stack-probe/`、截图 `probe-window.png`（未挂字体）与 `probe-window-cjk.png`（挂字体）

## 一、结论摘要

两条路线**都可行**（egui 那套依赖在本机离线环境里实测能从 cargo 缓存构建出来），
但代价差一个量级。**推荐 v1.0 用原生 Win32**，最硬的两条理由是：

1. **内存差 12~15 倍**（14.3 MB vs 175~212 MB）——
   CanLapse 的定位是**和 SAI2/PS/Krita 长期并存的常驻工具**，
   它多占的内存和 GPU 就是绘画软件少掉的。
2. **中文开箱不可用**：egui 自带字体只有拉丁字形，整个中文界面是豆腐块（有截图为证）；
   必须外挂系统 CJK 字体才正常，且外挂后内存再涨 37 MB、emoji 仍会退化成方块/单色小字形。

但如果优先级是"最快看到完整界面、且不介意外挂字体与内存占用"，egui 更快 ——
`add/egui.rs` 那 154 行已经覆盖了整页结构。

## 二、实测数据

| 指标 | egui / eframe 探针 | 原生 Win32（Rust） |
|---|---|---|
| release exe | **13.86 MB** | **654 KB**（canlapse.exe，已含 ffmpeg 链接） |
| 空闲工作集内存 | **175.3 MB** → 挂字体后 **212.2 MB** | **14.3 MB**（捕获 spike 的 Win32 窗口实测） |
| 额外运行时 DLL | 无（用系统 `opengl32`） | 编码需 6 个（5 个 `av*` + `libwinpthread-1.dll`） |
| 空闲 CPU（单核占比） | 0.3%（响应式重绘，不高） | ~0% |
| 依赖 crate 数 | **418 个包** | 24 个包 |
| 首次构建 | 418 个 crate 全量编译（数分钟） | 秒级 |
| 中文开箱 | **❌ 全是豆腐块** | ✅ 系统字体天然正常 |
| emoji 图标 | ⚠️ 挂字体后仍只有单色小字形（🎯🏷⏹ 会退化） | ✅ 用系统字体/图标，可控 |
| 无边框置顶悬浮条 | 需额外 viewport，且**是否抢焦点需实测**（winit 行为） | ✅ `WS_EX_NOACTIVATE|TOPMOST|TOOLWINDOW` + `WM_NCHITTEST` |
| 现成控件 | ✅ 折叠面板/滑块/下拉/进度条全有 | ❌ 需用 Trackbar/ComboBox/ListView 自己搭折叠面板 |
| 你的 `add/egui.rs` 草稿 | ❌ **按旧 API 写的，编译不过**（见下） | 不适用 |

内存那两行是同一台机器、同一次会话里实测的：egui 探针 `WorkingSet64` = 175.3 MB（挂字体后 212.2 MB），
原生 Win32 窗口 = 14.3 MB。

## 三、一个必须先解决的兼容问题

`add/egui.rs` 第一行是：

```rust
fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
```

但本机唯一可离线构建的 **egui 0.36.2** 里，`eframe::App` **已经没有 `update` 了**：

```
error[E0407]: method `update` is not a member of trait `eframe::App`
help: implement the missing item: `fn ui(&mut self, _: &mut Ui, _: &mut eframe::Frame)`
```

0.36 的 trait 是 `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame)`，
并且 `CentralPanel::show` 也从收 `&egui::Context` 改成了收 `&mut Ui`。
也就是说：**草稿不是"抄进去就能跑"，整页结构要按 0.36 的 API 重排**
（`ctx` → `ui`，`Window::show` 才仍需 `&Context`）。这不是大改，但也不是零成本。

## 四、逐项权衡

### egui 的优势
* **布局代码量小**：154 行草稿就搭出整页（含两个折叠面板、滑块、复选、会话列表、进度条）。
* **控件齐全且现代**：`CollapsingHeader`/`Slider`/`ComboBox`/`ProgressBar` 开箱即用，
  原生 Win32 每一样都要自己接。
* **视觉风格统一**，深色主题现成，不需要自己画。
* 无额外 DLL 需要分发（走系统 OpenGL）。

### egui 的代价
* **内存 12~15 倍**：对"和绘画软件抢资源"的常驻工具是硬伤。
* **exe 大 21 倍**：与"小巧"这个明确设计约束直接冲突。
* **中文/emoji 需要额外集成**：外挂系统字体（可行，已验证）或子集化嵌入；
  注意嵌入 `msyh` 有再分发限制，加载系统字体则不能假设目标机器一定有某个字体。
* **418 个依赖包**：离线可构建（已验证），但供应链与构建时间成本实在。
* **悬浮条是产品关键交互**：无边框 + 置顶 + **不抢焦点** + 可拖动。
  Win32 有精确控制手段；egui 要用第二个 viewport，**抢焦点行为需要实测**，
  一旦它会抢焦点，艺术家在画布上落笔时焦点被夺走 —— 这是功能性风险，不只是体验问题。

### 原生 Win32 的优势
* **资源占用低一个量级**，长时间挂机不干扰绘画软件。
* **中文/emoji 零成本**（系统字体）。
* **exe 小**，符合"小巧"约束；单一 exe + 少量 DLL 即可分发。
* **悬浮条与主窗口最小化**都是原生能力，与既有代码风格一致
  （`src/win.rs`、`src/capture.rs` 已经是这套写法）。
* 与 DWM 缩略图抓帧天然同栈（都是 Win32），不需要在事件循环里协调两套线程模型。

### 原生 Win32 的代价
* **代码量大**：折叠面板要自己实现（点标题 → 显隐子控件 + 重排），
  滑块/下拉/列表/进度条要用 `msctls_trackbar32` / `COMBOBOX` / `ListView` / `msctls_progress32`。
  估计整页 **800~1200 行**（这是估算，不是实测）。
* **迭代慢**：改布局要重新编译再截图验证；egui 改一行即时可见。
* **视觉是系统原生风格**，不如 egui 现代；要做深色主题得自己处理。

## 五、建议

**v1.0 走原生 Win32。** 决定性理由是"常驻工具的资源占用"与"中文/emoji 开箱可用"，
这两条恰好是 egui 最弱、Win32 最强的地方；而 egui 的长处（布局代码量、现成控件）
在 460×380 单页这种规模下是一次性成本，之后不再增长。

如果最终仍想用 egui，需要接受并落实三件事：
1. 外挂 CJK 字体（并决定是加载系统字体还是子集化嵌入）；
2. 内存基线从 ~15 MB 变成 ~200 MB；
3. 先实测"无边框置顶 viewport 会不会抢走绘画软件的焦点"，这一条不通过就不能用于产品。

## 六、复现方式

```powershell
cd spikes\ui-stack-probe
cargo build --offline --release          # 实测能从离线缓存构建（418 个包）
.\target\release\ui_stack_probe.exe      # 不挂字体 -> 中文全是豆腐块
# 挂系统字体（程序内自动尝试 msyh.ttc/simhei.ttf/simsun.ttc）
```

`probe-window.png` = 未挂字体（豆腐块）；`probe-window-cjk.png` = 挂了 `msyh.ttc`（中文正常）。
