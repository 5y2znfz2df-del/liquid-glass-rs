# 用 Windows 自带的 C# 编译器生成单文件安装包（液态玻璃番茄钟）。
#
#   .\build-installer.ps1                 正常构建
#   .\build-installer.ps1 -NoPayloadZip   不重新压 payload（改 Installer.cs 后用，快很多）
#
# 产物：..\dist\LiquidGlassPomodoro-Setup.exe
param([switch]$NoPayloadZip)

$ErrorActionPreference = 'Stop'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $here
$dist = Join-Path $root 'dist'
$app = Join-Path $dist 'LiquidGlassPomodoro'
$staging = Join-Path $here 'staging'
$payload = Join-Path $staging 'payload.zip'
$output = Join-Path $dist 'LiquidGlassPomodoro-Setup.exe'
$exe = Join-Path $root 'target\release\liquid-glass-rs.exe'

# ---- 1) 摆好要装进去的文件 ----
if (-not (Test-Path $exe)) {
    throw "找不到 $exe，先 cargo build --release"
}
if (Test-Path $app) { Remove-Item $app -Recurse -Force }
New-Item -ItemType Directory -Force -Path $app | Out-Null
Copy-Item $exe (Join-Path $app 'LiquidGlassPomodoro.exe')
foreach ($name in @('README.md', 'LICENSE', 'icon.ico', 'final-frame.png')) {
    $p = Join-Path $root $name
    if (Test-Path $p) { Copy-Item $p $app }
}

# ---- 2) 压成 payload.zip（顶层目录名会被安装器剥掉）----
New-Item -ItemType Directory -Force -Path $staging | Out-Null
if (-not $NoPayloadZip -or -not (Test-Path $payload)) {
    Write-Host '正在压缩应用文件…'
    if (Test-Path $payload) { Remove-Item $payload -Force }
    Compress-Archive -Path $app -DestinationPath $payload -CompressionLevel Optimal -Force
}
Write-Host ("payload: {0:N1} MB" -f ((Get-Item $payload).Length / 1MB))

# ---- 3) 编译 ----
$csc = Join-Path $env:SystemRoot 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
if (-not (Test-Path $csc)) { $csc = Join-Path $env:SystemRoot 'Microsoft.NET\Framework\v4.0.30319\csc.exe' }
if (-not (Test-Path $csc)) { throw '找不到 .NET Framework 的 csc.exe' }
$fw = Split-Path -Parent $csc

$refs = @(
    'System.dll', 'System.Core.dll', 'System.Windows.Forms.dll', 'System.Drawing.dll',
    'System.IO.Compression.dll', 'System.IO.Compression.FileSystem.dll', 'Microsoft.CSharp.dll'
)
$cscArgs = @()
$cscArgs += '/target:winexe'
$cscArgs += '/codepage:65001'
$cscArgs += '/optimize+'
$cscArgs += '/nologo'
foreach ($name in $refs) { $cscArgs += ('/r:' + (Join-Path $fw $name)) }
$cscArgs += ('/resource:' + $payload)
$icon = Join-Path $root 'icon.ico'
if (Test-Path $icon) { $cscArgs += ('/win32icon:' + $icon) }
$cscArgs += ('/out:' + $output)
$cscArgs += (Join-Path $here 'Installer.cs')

Write-Host '正在编译安装程序…'
& $csc $cscArgs
if ($LASTEXITCODE -ne 0) { throw "csc 编译失败（退出码 $LASTEXITCODE）" }

$f = Get-Item $output
Write-Host ("生成: {0}  ({1:N2} MB)" -f $output, ($f.Length / 1MB))
Write-Host ("SHA256: {0}" -f (Get-FileHash $output -Algorithm SHA256).Hash)
