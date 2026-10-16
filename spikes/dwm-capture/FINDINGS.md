# DWM 缩略图抓帧可行性原型 · 实测结论

> 位置：`spikes/dwm-capture/`（原型，不属于正式产品代码）
> 日期：2026-02-14
> 目的：回答一个**只能靠实测回答**的问题 —— DWM 窗口缩略图能不能被程序读到像素？以及它是否满足
> 「画布窗口被其他窗口盖住时仍能继续录制」这个产品核心诉求。

---

## 一、结论摘要（先看这里）

| 读回方式 | 目标可见 | 目标被完全遮挡 | 目标已最小化 | 能否用于产品 |
|---|---|---|---|---|
| M1 屏幕 `BitBlt` | ✅ | ❌ 抓到的是遮挡窗口 | ❌ 尺寸退化为 0×0 | 不能作为主方案 |
| M2 `PrintWindow(源, PW_RENDERFULLCONTENT)` | ✅ | ✅ | ❌ 全黑 | 不能作为主方案 |
| M3 `PrintWindow(源, 0)` | ⚠️ 部分应用失败 | ⚠️ | ❌ 全黑 | 不能作为主方案 |
| **M4 缩略图宿主 + `PrintWindow(宿主, PW_RENDERFULLCONTENT)`** | ✅ | ✅ | ✅ | **★ 选它** |
| M5 缩略图宿主 `BitBlt`（`GetDC(宿主)`） | ❌ 只有宿主自绘底色 | ❌ | ❌ | 不可用 |
| M7 缩略图宿主矩形的屏幕抓取 | ✅ | ⚠️ 宿主被别人盖住就失效 | ✅ | 仅作诊断参考 |

**核心结论：**

1. **DWM 缩略图可以读到像素**，但取像素的不是 `BitBlt`，而是对**注册了缩略图的宿主窗口**调用
   `PrintWindow(host, hdc, PW_RENDERFULLCONTENT(0x2))`。这是唯一一个在
   「可见 / 被完全遮挡 / 源窗口已最小化」三种状态下都成立的读回方式。
2. **`BitBlt` 永远读不到缩略图**（M5 恒定为宿主自绘底色，实测 100% 品红，均值 170.0）。
   这与 DWM 的已知行为一致：缩略图是合成器画上去的，不在窗口自己的 DC 内容里。
3. **屏幕抓取（M1）在目标被遮挡时必然失效**，且目标最小化时客户区退化为 0×0。
4. **直接对源窗口 `PrintWindow`（M2/M3）在真实 GPU 合成应用上不可靠**：
   在 Chromium 窗口（`Chrome_WidgetWin_1`）上实测**直接返回失败(0)**，而同一时刻 M4 成功。
   在另一个进程的普通 GDI 窗口和 Qt 窗口上 M2 是成功的 —— 说明它具有**应用相关性**，不能当主方案。
5. **M4 抓的是「这个窗口自己的内容」，压在上面的其他窗口不会污染结果。**
   这一点在 Krita 上得到了一个很直观的旁证（见实验 4）。

---

## 二、实验设计（为什么这些结论可信）

原型不是"看看是不是黑屏"了事。它自建一个**按 tick 画确定性图案**的源窗口：

* 四象限纯色（红/绿/蓝/白）→ 用 40×40 采样块核对内容是否正确
* 16 位二进制计数条（黑/白格子按 tick 的二进制位画）→ 用位值核对"抓到的到底是哪一帧"
* 移动方块（位置由 `tick % 10` 决定）→ 提供帧间差异

抓帧后**逐像素核对**：象限命中率 ≥ 0.9 且 16 位位值**完全等于**预期 tick，才判定"确实抓到了该窗口该帧的内容"。
只统计"非空/非黑"是不够的 —— 那无法区分"抓到了目标"和"抓到了别的东西"。

---

## 三、实验 1：同进程自检（源/遮挡/宿主均为原型自建窗口）

命令：`dwm_capture_spike.exe`（默认自检模式），产物在 `out/`。

关键结果（完整表见运行输出）：

```
方法                                 状态          非空色数   象限得分   位值   位匹配
M1_screen_bitblt                   visible        8      1.00   272    是
M4_host_printwindow_fullcontent    visible        8      1.00   275    是
M5_host_bitblt                     visible        1      0.00     -     否     <- 均值 170.0 = 纯品红
M1_screen_bitblt                   occluded       1      0.00     0     否     <- 均值 24.0 = 遮挡窗口
M2_printwindow_fullcontent         occluded       8      1.00  4609    是
M4_host_printwindow_fullcontent    occluded       8      1.00  4611    是
M6_host_offscreen_printwindow      occluded       8      1.00  4614    是
```

* M1 在遮挡前正确、遮挡后变成遮挡窗口的深灰（均值 24.0）—— 说明**遮挡真的发生了**，对照关系自洽。
* **M6：把宿主窗口移到屏幕外 x = -4000 之后，M4 依然能取到正确内容。**
  说明宿主**不需要显示在屏幕上**，产品可以用一个离屏/不可见的宿主窗口。
* 视觉确认：`out/visible_M4_host_printwindow_fullcontent.png` 与
  `out/visible_M1_screen_bitblt.png` 均为源窗口图案；`out/visible_M7_host_screen_bitblt.png`
  拍到的却是一个**文件资源管理器窗口**（当时它盖在宿主窗口上）——这直接证明了
  M7（屏幕抓取宿主矩形）会被别的窗口污染，而 M4 不会。

---

## 四、实验 2：跨进程三态（决定性实验）

命令：`dwm_capture_spike.exe --cross --out out`

设计：主进程用 `--helper` 参数**另起一个独立进程**作为目标窗口（真正的跨进程），
该对端进程把当前 tick 写进**窗口标题**；主进程通过跨进程 `PostMessage` 精确设定 tick，
再从标题读回"它自称的 tick"作为预期值 —— 于是**跨进程条件下也能严格自证**。

实测（本进程 pid=6920，对端 pid=9964，`跨进程: true`）：

```
方法                                 状态       预期tick  读到位值  象限得分   均值   位匹配
M1_screen_bitblt                   normal       513     513     1.00   81.0    是
M2_printwindow_fullcontent         normal       513     513     1.00   81.0    是
M3_printwindow_plain               normal       513     513     1.00   81.0    是
M4_host_printwindow_fullcontent    normal       513     513     1.00   81.0    是
M5_host_bitblt                     normal       513       -     0.00  170.0    否
M7_host_screen_bitblt              normal       513     513     1.00   81.0    是
M1_screen_bitblt                   occluded     770       0     0.00   24.0    否
M2_printwindow_fullcontent         occluded     770     770     1.00   82.0    是
M3_printwindow_plain               occluded     770     770     1.00   82.0    是
M4_host_printwindow_fullcontent    occluded     770     770     1.00   82.0    是
M5_host_bitblt                     occluded     770       -     0.00  170.0    否
M7_host_screen_bitblt              occluded     770     770     1.00   82.0    是
M1_screen_bitblt                   minimized      -    失败: 尺寸非法 0x0
M2_printwindow_fullcontent         minimized    770       -     0.00    0.0    否   <- 全黑
M3_printwindow_plain               minimized    770       -     0.00    0.0    否   <- 全黑
M4_host_printwindow_fullcontent    minimized    770     770     1.00   82.0    是   <- ★
M5_host_bitblt                     minimized    770       -     0.00  170.0    否
M7_host_screen_bitblt              minimized    770     770     1.00   82.0    是
```

差异矩阵：

```
方法                                       正常 vs 遮挡   正常 vs 最小化
M1_screen_bitblt                            1.000            -
M2_printwindow_fullcontent                  0.032         -1.000
M4_host_printwindow_fullcontent             0.032          0.032
M5_host_bitblt                              0.000          0.000
```

**读法：**

* `IsIconic=1` —— 最小化真实发生。
* M1「正常 vs 遮挡 = 1.000」：屏幕抓取在遮挡后**每一个像素都变了**，即完全失效。
* M2/M4/M7「正常 vs 遮挡 = 0.032」：这 3.2% 正好等于 tick 由 513(0x201) 变为 770(0x302) 所引起的
  位条格子翻转 + 移动方块位移。反过来说：**遮挡期间缩略图仍在实时刷新，画面不是冻结的**。
* M4 在最小化状态下位值仍等于 770 且象限得分 1.00 → **源窗口最小化后依然能取到画面**。
* M2/M3 在最小化后变成全黑（均值 0.0，PNG 仅 12 字节）。
* M5 恒定 0.000 且均值恒为 170.0 → 永远只是宿主自己的品红底色。

---

## 五、实验 3：真实 GPU 合成应用（Chromium）

命令：`dwm_capture_spike.exe --title "WorkBuddy" --out out/foreign-workbuddy`
目标：`workbuddyai.exe` / `Chrome_WidgetWin_1`，窗口 1302×887

```
方法                                尺寸      非空色数   均值
M1_screen_bitblt                 1302x887     983    246.9
M2_printwindow_fullcontent        失败: 绘制调用返回失败(0)      <- ★ 直接 PrintWindow 失败
M3_printwindow_plain              失败: 绘制调用返回失败(0)      <- ★ 直接 PrintWindow 失败
M4_host_printwindow_fullcontent  1280x800     876    248.4        <- ★ 成功
M5_host_bitblt                   1280x800       1    170.0        <- 品红
M7_host_screen_bitblt            1280x800     877    248.4
```

`out/foreign-workbuddy/M4_host_printwindow_fullcontent.png` 经目视确认：**该 Chromium 窗口侧栏、正文、
任务列表全部清晰完整**，与屏幕上所见一致。

**这条证据很关键**：Chromium 是典型的 GPU 合成窗口，直接 `PrintWindow` 在它上面**直接失败**；
而 M4 却拿到了完整、干净的画面。说明 M4 不依赖目标应用是否支持 `WM_PRINT`，
而是走 DWM 合成器那一层 —— 这正是"目标应用无关"的实现路径。

---

## 六、实验 4：真实绘画软件 Krita（旁证）

环境限制说明：本机 Krita 5.3.4.1 在**本会话中启动即崩**，弹窗
「Krita：发生严重错误 —— 无法将 memory 与数据库同步，Krita 将会立即退出」。
极可能是本会话的**文件沙箱不允许它写 `%LOCALAPPDATA%\krita` 下的资源数据库**所致，
属环境限制，不是 CanLapse 的问题。因此**未能对 Krita 主窗口 + 画布做完整验证**。

但这次失败反而提供了一个有价值的旁证。此时被当作目标的 "Krita" 窗口其实是
**Krita 启动画面**（`Qt5157QWindowIcon`，960×480），而它的上面压着一个崩溃对话框：

* `out/foreign-krita/normal_M1_screen_bitblt.png`：启动画面 **+ 崩溃对话框**（屏幕抓取，被污染）
* `out/foreign-krita/normal_M4_host_printwindow_fullcontent.png`：**只有启动画面，没有崩溃对话框**

即 **M4 抓取的是「该窗口自身的内容」，压在它上面的其它顶层窗口不会进入画面**。
这正是缩时录屏产品最需要的行为（画布被聊天窗口/参考图盖住时照样录干净）。

同组数据里 `minimized_M4` 与 `normal_M4` 字节数完全相同（449,529），
而同状态下 `minimized_M2/M3` 只有 12 字节（空图）—— 与实验 2 的结论一致。

---

## 七、对产品设计的影响（结论落到 CanLapse 上）

1. **捕获模块按 M4 设计**：一个宿主顶层窗口 + `DwmRegisterThumbnail(host, canvasHWND)`
   + `DwmUpdateThumbnailProperties`（`DWM_TNP_RECTDESTINATION|RECTSOURCE|OPACITY|VISIBLE|SOURCECLIENTAREAONLY`），
   抓帧时对宿主调 `PrintWindow(..., PW_RENDERFULLCONTENT)`。
   `rcSource` 用源窗口客户区、`rcDestination` 用宿主客户区，并让**两处尺寸相同以保证 1:1 不缩放**。
2. **宿主窗口不需要可见**：实测移到屏幕外 4000px 仍然有效（M6）。因此产品可以完全不打扰用户。
   但为稳妥，仍建议保留"宿主极小/离屏"两种模式的开关，并在真机上回归验证。
3. **不要用屏幕抓取做兜底主路径**：目标被遮挡即失效，且最小化时客户区 0×0。
4. **`BitBlt` 读缩略图是死路**，不要再尝试。
5. **直接 `PrintWindow` 源窗口不能当主方案**：在 Chromium 上直接失败，应用相关性太强。
6. **目标窗口被最小化时仍可取到画面**（本原型实测成立）。
   但要注意这是"取到画面"，不代表"源应用还在渲染"——很多绘画软件最小化后会停止重绘，
   那样录出来会是**静止帧**。CanLapse 的正常流程是最小化**自己的主窗口**、不最小化画布，
   所以影响有限；但若以后做"自动最小化画布"之类的功能，必须逐应用实测。

---

## 八、未验证 / 已知风险（如实记录）

1. **超大画布**：本次最大只验证到 1302×887（Chromium）与 1280×800。
   4K 画布（3840×2160）下宿主窗口尺寸、缩略图是否 1:1、是否有 DWM 尺寸上限，**尚未验证**。
2. **桌面合成关闭（DWM 未启用）时**：本原型只在 `DwmIsCompositionEnabled=1` 下测过；
   关闭时 `DwmRegisterThumbnail` 的行为与降级路径**尚未验证**。
3. **Windows 版本差异**：本机为 Windows 11（Windows SDK 10.0.28000）。
   Windows 10 早期版本、或远程桌面/多用户会话下的行为**未验证**。
4. **SAI2 / Photoshop / CLIP STUDIO PAINT**：本机未安装，**完全未验证**。
   建议后续用真机 + 真画布窗口补测，尤其是 Photoshop（GPU 合成）与 CSP。
5. **`PW_RENDERFULLCONTENT` 对宿主窗口的语义**：本次是黑盒实测 —— 观察到它能取到合成后的缩略图，
   但微软没有为"缩略图 + PrintWindow"这一组合做出明确文档承诺，属于**依赖实现行为**。
   正式实现应加入自检：抓到全黑/恒定画面时给出告警并可切换到别的模式。
6. **Krita 主窗口 + 浮动画布未验证**（原因见实验 4，属环境限制）。

---

## 九、复现步骤

```powershell
cd D:\deepseek-harness\default-workspace\CanLapse\spikes\dwm-capture

# 1) 同进程自检（自建源/遮挡/宿主窗口，图案自证）
cargo build --offline
.\target\debug\dwm_capture_spike.exe --out out

# 2) 跨进程 + 遮挡 + 最小化（决定性实验，自动起 --helper 子进程）
.\target\debug\dwm_capture_spike.exe --cross --out out

# 3) 对任意真实窗口跑三态（正常/遮挡/最小化）
.\target\debug\dwm_capture_spike.exe --list
.\target\debug\dwm_capture_spike.exe --foreign --title "记事本" --out out\foreign-xxx
.\target\debug\dwm_capture_spike.exe --foreign --hwnd 0x1234 --out out\foreign-xxx

# 4) 对任意真实窗口只跑一轮，看各方式是否成功
.\target\debug\dwm_capture_spike.exe --hwnd 0x1234 --out out\single
```

依赖：Rust 1.99（`x86_64-pc-windows-msvc`）、`cargo build --offline`
（本机 crates.io 不通，依赖必须来自本地 registry 缓存：`windows-sys 0.61.2`、`png 0.18.1` 及其传递依赖）。

**注意**：运行时会短暂创建几个可见测试窗口（源窗口、遮挡窗口、缩略图宿主），
跑完自动退出；`--foreign` 会短暂遮挡并最小化目标窗口，结束后恢复。

---

## 十、产物清单

| 路径 | 内容 |
|---|---|
| `src/main.rs` | 原型源码（含自证图案、6 种读回方式、三态测试、`--helper` 对端进程） |
| `out/visible_*.png`、`out/occluded_*.png` | 同进程自检每种方式的截图 |
| `out/foreign-workbuddy/*.png` | Chromium（GPU 合成）上的各方式结果 |
| `out/foreign-krita/*.png` | Krita 启动画面上的各方式结果（含"对话框未污染 M4"的证据） |
| `out/desktop.png` | 当时桌面全屏截图（用于定位异常窗口） |
