# CanLapse 打包脚本：把 release 产物整理成 zip
#
# 为什么单独一个脚本：
#   * v1.2.0 起 ffmpeg 是**静态链接** + 静态 CRT，交付物就是**单个 exe**；
#     release 面向所有用户，**只发 GUI**（canlapse-cli.exe 仅供开发/排障，不随包）。
#   * 这里固定"只放 CanLapse.exe + 使用说明.txt"，防止历史 DLL 或 CLI 被误拷进去。
#
# 用法：pwsh -File tools\package.ps1 [-Version 1.2.1]
param([string]$Version = '1.2.1')

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$exe = Join-Path $root 'target\release\CanLapse.exe'
if (-not (Test-Path $exe)) {
    throw "缺少 release 产物：先跑 pwsh -File tools\build.ps1 -Profile release"
}

$dist = Join-Path $root "dist\CanLapse-v$Version-win64"
Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $dist -Force | Out-Null
Copy-Item $exe $dist

# 使用说明：仓库里维护一份（tools\使用说明.txt），包里只改版本号
$readme = Join-Path $root 'tools\使用说明.txt'
if (Test-Path $readme) {
    $txt = Get-Content $readme -Raw -Encoding UTF8
    $txt = $txt -replace 'CanLapse v\d+\.\d+\.\d+', "CanLapse v$Version"
    Set-Content (Join-Path $dist '使用说明.txt') -Value $txt -Encoding UTF8 -NoNewline
}

$zip = Join-Path $root "dist\CanLapse-v$Version-win64.zip"
Remove-Item $zip -Force -ErrorAction SilentlyContinue
Compress-Archive -Path $dist -DestinationPath $zip -CompressionLevel Optimal

Write-Host "打包完成：$zip"
Get-ChildItem $dist | Select-Object Name, Length | Format-Table -AutoSize
Write-Host ("SHA256: " + (Get-FileHash $zip -Algorithm SHA256).Hash)
