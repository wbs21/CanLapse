# CanLapse 构建后资源注入（图标 + 版本信息）
#
# 用法：pwsh -File tools\embed-resources.ps1
# 时机：**每次 cargo build 之后**都要跑一次 —— 构建会重新生成 exe，覆盖掉注入的资源。
#
# 背景见 tools\ResInject.cs 与 CanLapse.rc 的注释：链接期方案在本项目行不通，
# 因此改为构建后注入；.rc 必须是 UTF-16LE 且不能有 #pragma code_page。
param(
    # 产物目录（相对项目根）。**不要改成 [string[]]$Exe**：
    # `pwsh -File script.ps1 -Exe @(a,b)` 会把数组压成单个值，
    # 结果只处理第一个文件（实测踩过：canlapse-cli.exe 的图标静默丢失）。
    [string]$Dir = 'target\debug'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Find-Tool([string]$Pattern) {
    Get-ChildItem $Pattern -ErrorAction SilentlyContinue | Sort-Object FullName | Select-Object -Last 1
}

$rc = Find-Tool 'C:\Program Files (x86)\Windows Kits\10\bin\10.*\x64\rc.exe'
$link = Find-Tool 'C:\Program Files*\Microsoft Visual Studio\*\*\VC\Tools\MSVC\*\bin\Hostx64\x64\link.exe'
if (-not $rc) { throw '找不到 rc.exe（Windows SDK）' }
if (-not $link) { throw '找不到 link.exe（MSVC）' }

$tmp = Join-Path $root '.testcache\resembed'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

& $rc.FullName /nologo /fo "$tmp\CanLapse.res" "$root\CanLapse.rc"
if ($LASTEXITCODE -ne 0) { throw "rc.exe 失败（退出码 $LASTEXITCODE）" }

# 这道校验就是为了防住那个静默失败：rc.exe 退出码为 0，但 .rc 若是 UTF-8 且带
# #pragma code_page，产出的 .res 会只有几百字节（几乎为空），后续全都"看起来成功"却拿不到资源。
$size = (Get-Item "$tmp\CanLapse.res").Length
if ($size -lt 4096) {
    throw ".res 只有 $size 字节，几乎肯定是 rc.exe 静默失败 —— 检查 CanLapse.rc 是否为 UTF-16LE 且没有 #pragma code_page"
}

& $link.FullName /nologo /dll /noentry /machine:x64 "/out:$tmp\res.dll" "$tmp\CanLapse.res"
if ($LASTEXITCODE -ne 0) { throw "link.exe 失败（退出码 $LASTEXITCODE）" }

Add-Type -Path (Join-Path $PSScriptRoot 'ResInject.cs')

$targets = @(
    (Join-Path $root (Join-Path $Dir 'CanLapse.exe')),
    (Join-Path $root (Join-Path $Dir 'canlapse-cli.exe'))
)
$done = 0
foreach ($path in $targets) {
    if (-not (Test-Path $path)) { Write-Warning "跳过（不存在）: $path"; continue }
    $result = [ResInject]::Inject("$tmp\res.dll", $path)
    Write-Host ("注入 {0}: {1}" -f (Split-Path $path -Leaf), $result)
    $done++
}
if ($done -eq 0) { throw "没有任何 exe 被注入（检查 -Dir 是否正确：$Dir）" }