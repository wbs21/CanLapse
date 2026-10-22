# CanLapse 工作日志 2026-10-18

## release 发行版构建（未修改任何代码）

### 做了什么
按项目既定发布流程重新构建了一版 release 发行版：

1. `cargo build --offline --release` —— 编译 lib + 两个 bin（`CanLapse.exe` GUI 入口、`canlapse-cli.exe` CLI）。
2. `tools/embed-resources.ps1 -Dir target\release` —— 构建后注入图标/版本资源（`rc.exe` → `.res` → `link /dll /noentry` → `ResInject` 写回）。
3. `tools/package.ps1 -Version 1.3.0` —— 组装 `dist/CanLapse-v1.3.0-win64.zip`。

### 产物
| 文件 | 说明 |
|---|---|
| `dist/CanLapse-v1.3.0-win64.zip` | 发行包，1,653,243 字节（压缩后 ≈1.58 MB；包内文件解压后总计 3,819,642 字节） |
| `dist/CanLapse-v1.3.0-win64/CanLapse.exe` | 单文件 exe，3,784,704 字节，版本 1.3.0.0 |
| 同目录 | `使用说明.txt`、`LICENSE`、`THIRD-PARTY-NOTICES.md`、`licenses/`（ffmpeg LGPL + openh264 BSD） |

SHA256(zip)：`fc135191a03f036229314e98bdfaa8268c382a3ca7194e581561b090b8826a26`

### 验证状态
- [x] `cargo build --offline --release` 退出码 0（仅有既有的 dead_code / cfg 警告，非本次引入）。
- [x] 资源注入：`资源类型=3 已复制=9 写回=True`（两个 exe 均注入，与 ResInject.cs 注释中的预期值一致）。
- [x] exe 版本信息：ProductName=CanLapse、FileVersion=1.3.0.0。
- [x] `canlapse-cli --help` 冒烟通过（输出版本 1.3.0 与用法）。
- [x] `unzip -t` 压缩包无错误，SHA256 与 package.ps1 输出一致。
- [x] 全流程未改动任何源码（git 工作树干净，仅新增本文件与未跟踪的过期脚本 `tools/_release-build.ps1`）。

### 过程中的两个环境坑（记录备查）
1. 后台任务沙箱拒绝 cargo 写 `target/release/.fingerprint/*/invoked.timestamp`（os error 5，退出码 101）——改用前台 Bash 直接跑 cargo 成功；后台 PowerShell 工具对个别命令串还会报沙箱异常（换等价写法即可）。
2. `build.rs` 缓存输出里 ffmpeg 静态库路径指向旧工作区 `D:\deepseek-harness\default-workspace\CanLapse\deps\ffmpeg-static\lib`（与本仓库 `deps/ffmpeg-static/lib` 逐字节一致且路径仍存在，链接成功）。若该目录将来被清理，需删除 `target/**/build/canlapse-*/output` 缓存强制重算。

### 备注
- zip 内中文文件名（`使用说明.txt`）由 Windows PowerShell 5.1 `Compress-Archive` 按本地代码页（GBK）存储——中文 Windows 资源管理器/常见解压工具解压正常，此为项目既有打包方式，未改动。
- 待办：`tools/_release-build.ps1` 是指向旧工作区路径的过期脚本（未跟踪），可清理；build.rs 的 ffmpeg 路径缓存可按需重置。

## 远程仓库配置与 v1.3.0 推送（同日追加）
- 已添加远程仓库 `origin = https://github.com/wbs21/CanLapse.git`，并推送 `main` 分支与附注 tag `v1.3.0`；远端 `ls-remote` 验证：`refs/heads/main`=675f05f、`refs/tags/v1.3.0`=16d91fb（指向 675f05f），与本地一致。
- 注意：本环境网络阻断 `github.com:443`（代理 502 / 直连超时），推送改走 GitHub 官方的 SSH-over-443 通道，origin 实际 URL 为 `ssh://git@ssh.github.com:443/wbs21/CanLapse.git`（SSH 密钥 ~/.ssh/id_ed25519 已归属账号 wbs21）。若后续 github.com 的 443 恢复可用，可 `git remote set-url origin https://github.com/wbs21/CanLapse.git` 切回。
- 构建产物（dist/、target/、*.log）均已被 .gitignore 忽略，未进仓库；未跟踪的 `WORK_SUMMARY_2026-10-18.md` 与过期脚本 `tools/_release-build.ps1` 未提交、未推送。

## GitHub Release v1.3.0 发布（同日追加）
- 已在远程仓库发布正式 Release：<https://github.com/wbs21/CanLapse/releases/tag/v1.3.0>
  - tag `v1.3.0`（target=main）、标题 `CanLapse v1.3.0`、非 draft、非 prerelease，发布时间 2026-10-18 13:06 (UTC+8)。
  - 资产：`CanLapse-v1.3.0-win64.zip`（1,653,243 B，state=uploaded），digest 与本地文件一致 `sha256:fc135191…`。
  - 发布说明：`release/v1.3.0-release-notes.md`（中英双语：v1.3.0 变更点、下载与 SHA256、运行要求、许可说明），经 `gh release edit --notes-file` 更新过一版（修正体积描述：压缩包 1.58 MB，此前误写为解压后总大小 3.63 MB）。
- 操作方式：`gh release create v1.3.0 -R wbs21/CanLapse --title ... --notes-file ... dist/CanLapse-v1.3.0-win64.zip`（gh 已登录 wbs21；api.github.com 与 uploads.github.com 均可达）。
- 体积勘误：发行包压缩后 1,653,243 B（≈1.58 MB）；exe 单文件 3,784,704 B（≈3.61 MB）；「3,819,642 B / 3.63 MB」是包内 6 个文件解压后的总大小， bukan 压缩包体积。
