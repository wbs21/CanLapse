# CanLapse 一键构建：cargo build + 构建后资源注入（图标 / 版本信息）
#
# 为什么需要这个包装脚本：
#   资源是**构建后**注入的（链接期方案在本项目行不通，原因见 tools\ResInject.cs），
#   而 `cargo build` 会重新生成 exe 从而覆盖掉已注入的资源 ——
#   两者必须成对执行，否则"图标突然没了"。
#
# 用法：
#   pwsh -File tools\build.ps1                 # debug 构建 + 注入
#   pwsh -File tools\build.ps1 -Profile release
#   pwsh -File tools\build.ps1 -SkipInject     # 只构建（例如做纯编译检查）
param(
    [ValidateSet('debug', 'release')] [string]$Profile = 'debug',
    [switch]$SkipInject
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$cargoArgs = @('build', '--offline')
if ($Profile -eq 'release') { $cargoArgs += '--release' }

Write-Host ("> cargo " + ($cargoArgs -join ' '))
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { throw "cargo build 失败（退出码 $LASTEXITCODE）" }

if ($SkipInject) {
    Write-Host '已按参数跳过资源注入'
    exit 0
}

$exeDir = if ($Profile -eq 'release') { 'target\release' } else { 'target\debug' }
& pwsh -NoProfile -File (Join-Path $PSScriptRoot 'embed-resources.ps1') -Dir $exeDir
if ($LASTEXITCODE -ne 0) { throw '资源注入失败' }

Write-Host '完成：已构建并注入图标 / 版本信息'
