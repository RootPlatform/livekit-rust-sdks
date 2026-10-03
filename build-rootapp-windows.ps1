param(
    [string[]]$Targets = @('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc'),
    [string]$OutDir = "$PSScriptRoot\rootapp-dist"
)

$ErrorActionPreference = 'Stop'
$tools = "$env:USERPROFILE\dev-tools"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$tools\protoc\bin;$env:PATH"
$env:PROTOC = "$tools\protoc\bin\protoc.exe"
$libclang = Get-ChildItem "$tools\llvm" -Recurse -Filter libclang.dll | Select-Object -First 1
if (-not $libclang) { throw "libclang.dll not found under $tools\llvm" }
$env:LIBCLANG_PATH = $libclang.DirectoryName

Set-Location $PSScriptRoot
$rids = @{ 'x86_64-pc-windows-msvc' = 'win-x64'; 'aarch64-pc-windows-msvc' = 'win-arm64' }

foreach ($target in $Targets) {
    Write-Host "=== building livekit-ffi for $target ==="
    cargo build --release -p livekit-ffi --target $target
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed for $target" }
    $rid = $rids[$target]
    $dest = Join-Path $OutDir "$rid\native"
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item "target\$target\release\livekit_ffi.dll" $dest -Force
    Write-Host "copied -> $dest\livekit_ffi.dll"
}
