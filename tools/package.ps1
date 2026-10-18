# CanLapse 打包脚本：把 release 产物整理成 zip
#
# 为什么单独一个脚本：v1.2.0 起 ffmpeg 是**静态链接** + 静态 CRT，
# 交付物就是**单个 exe**（不再有 7 个 DLL）。这里固定"只放 exe + 使用说明"，
# 防止有人顺手把历史 DLL 又拷进去。
#
# 用法：pwsh -File tools\package.ps1 [-Version 1.2.0]
param([string]$Version = '1.2.0')

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$rel = Join-Path $root 'target\release'
$exe = Join-Path $rel 'CanLapse.exe'
$cli = Join-Path $rel 'canlapse-cli.exe'
if (-not (Test-Path $exe) -or -not (Test-Path $cli)) {
    throw "缺少 release 产物：先跑 pwsh -File tools\build.ps1 -Profile release"
}

$dist = Join-Path $root "dist\CanLapse-v$Version-win64"
Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $dist -Force | Out-Null
Copy-Item $exe $dist
Copy-Item $cli $dist

# 使用说明：沿用上一版的文本（去掉"需要 DLL"的过时说明）
$prev = Get-ChildItem (Join-Path $root 'dist') -Directory -Filter 'CanLapse-v*-win64' |
    Where-Object { $_.Name -ne (Split-Path $dist -Leaf) } |
    Sort-Object Name -Descending | Select-Object -First 1
$readme = if ($prev -and (Test-Path (Join-Path $prev.FullName '使用说明.txt'))) {
    Get-Content (Join-Path $prev.FullName '使用说明.txt') -Raw -Encoding UTF8
} else { '' }
if ($readme) {
    $readme = $readme -replace 'CanLapse v\d+\.\d+\.\d+', "CanLapse v$Version"
    $readme = $readme -replace '（DLL 必须同目录）', '（单文件，无需任何 DLL）'
    Set-Content (Join-Path $dist '使用说明.txt') -Value $readme -Encoding UTF8 -NoNewline
}

$zip = Join-Path $root "dist\CanLapse-v$Version-win64.zip"
Remove-Item $zip -Force -ErrorAction SilentlyContinue
Compress-Archive -Path $dist -DestinationPath $zip -CompressionLevel Optimal

Write-Host "打包完成：$zip"
Get-ChildItem $dist | Select-Object Name, Length | Format-Table -AutoSize
Write-Host ("SHA256: " + (Get-FileHash $zip -Algorithm SHA256).Hash)
