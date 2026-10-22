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
#   pwsh -File tools\build.ps1 -Profile release -SignThumbprint <证书指纹>
param(
    [ValidateSet('debug', 'release')] [string]$Profile = 'debug',
    [switch]$SkipInject,
    [string]$SignThumbprint = $env:CANLAPSE_SIGN_THUMBPRINT,
    [string]$TimestampUrl = 'http://timestamp.digicert.com'
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

if (-not [string]::IsNullOrWhiteSpace($SignThumbprint)) {
    $signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Source
    if (-not $signtool) {
        $programFilesX86 = [Environment]::GetFolderPath('ProgramFilesX86')
        if ([string]::IsNullOrWhiteSpace($programFilesX86)) {
            $programFilesX86 = [Environment]::GetFolderPath('ProgramFiles')
        }
        $kits = Join-Path $programFilesX86 'Windows Kits\10\bin'
        if (Test-Path $kits) {
            $signtool = Get-ChildItem $kits -Filter signtool.exe -Recurse -File |
                Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
                Sort-Object FullName -Descending |
                Select-Object -First 1 -ExpandProperty FullName
        }
    }
    if (-not $signtool) { throw '已指定签名证书，但找不到 signtool.exe（请安装 Windows SDK）' }

    $thumbprint = $SignThumbprint.Replace(' ', '')
    foreach ($name in @('CanLapse.exe', 'canlapse-cli.exe')) {
        $file = Join-Path $exeDir $name
        & $signtool sign /sha1 $thumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 $file
        if ($LASTEXITCODE -ne 0) { throw "签名失败：$file" }
        $signature = Get-AuthenticodeSignature -LiteralPath $file
        if ($signature.Status -ne 'Valid') {
            throw "签名验证失败：$file（$($signature.Status)）"
        }
    }
    Write-Host '完成：已构建、注入资源并完成 Authenticode 签名'
} else {
    Write-Host '完成：已构建并注入图标 / 版本信息（未签名）'
}
