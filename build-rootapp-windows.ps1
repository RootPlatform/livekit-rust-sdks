param(
    [string[]]$Targets = @('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc'),
    [string]$OutDir = "$PSScriptRoot\rootapp-dist",
    [string]$Arm64CrtLibDir = $env:LK_ARM64_CRT_LIB_DIR,
    [switch]$CompileOnly
)

$ErrorActionPreference = 'Stop'
$tools = "$env:USERPROFILE\dev-tools"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$tools\protoc\bin;$env:PATH"
$env:PROTOC = "$tools\protoc\bin\protoc.exe"
$libclang = Get-ChildItem "$tools\llvm" -Recurse -Filter libclang.dll | Select-Object -First 1
if (-not $libclang) { throw "libclang.dll not found under $tools\llvm" }
$env:LIBCLANG_PATH = $libclang.DirectoryName
$llvmBin = $libclang.DirectoryName

Set-Location $PSScriptRoot
$rids = @{ 'x86_64-pc-windows-msvc' = 'win-x64'; 'aarch64-pc-windows-msvc' = 'win-arm64' }

function Get-VsInstalls {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) { throw "vswhere.exe not found; install Visual Studio or Build Tools" }
    & $vswhere -all -products * -property installationPath
}

function Get-MsvcToolDirs {
    foreach ($vs in Get-VsInstalls) {
        Get-ChildItem "$vs\VC\Tools\MSVC" -Directory -ErrorAction SilentlyContinue |
            Where-Object { Test-Path "$($_.FullName)\include\vcruntime.h" } |
            Sort-Object { [version]$_.Name } -Descending
    }
}

function Test-NativeArm64Msvc {
    foreach ($dir in Get-MsvcToolDirs) {
        if ((Test-Path "$($dir.FullName)\bin\Hostx64\arm64\cl.exe") -and
            (Test-Path "$($dir.FullName)\lib\arm64\libcmt.lib")) { return $true }
    }
    return $false
}

$arm64CrtLibs = @('libcmt.lib', 'libvcruntime.lib', 'libcpmt.lib', 'oldnames.lib', 'delayimp.lib')

function Find-Arm64CrtDir {
    if ($Arm64CrtLibDir) { return $Arm64CrtLibDir }
    foreach ($dir in Get-MsvcToolDirs) {
        $candidate = "$($dir.FullName)\lib\arm64"
        if (Test-Path "$candidate\libcmt.lib") { return $candidate }
    }
    return $null
}

function Get-Arm64CrossEnv {
    $clangCl = Join-Path $llvmBin 'clang-cl.exe'
    $lldLink = Join-Path $llvmBin 'lld-link.exe'
    $llvmLib = Join-Path $llvmBin 'llvm-lib.exe'
    foreach ($tool in @($clangCl, $lldLink, $llvmLib, (Join-Path $llvmBin 'clang.exe'))) {
        if (-not (Test-Path $tool)) { throw "$tool not found; the arm64 cross build needs LLVM under $tools\llvm" }
    }

    $msvc = Get-MsvcToolDirs | Select-Object -First 1
    if (-not $msvc) { throw "no MSVC toolset with headers found" }

    $kits = "${env:ProgramFiles(x86)}\Windows Kits\10"
    $sdk = Get-ChildItem "$kits\Lib" -Directory |
        Where-Object { Test-Path "$($_.FullName)\um\arm64\kernel32.lib" } |
        Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
    if (-not $sdk) { throw "no Windows SDK with arm64 libraries under $kits\Lib" }
    $sdkInc = "$kits\Include\$($sdk.Name)"

    $crtDir = Find-Arm64CrtDir
    $missing = if ($crtDir) { $arm64CrtLibs | Where-Object { -not (Test-Path (Join-Path $crtDir $_)) } } else { $arm64CrtLibs }
    if ($missing -and -not $CompileOnly) {
        throw ("The ARM64 MSVC runtime libraries are missing ($($missing -join ', ')" +
            $(if ($crtDir) { " in $crtDir" } else { '' }) + "). Install the Visual Studio component " +
            "'MSVC v143+ ARM64/ARM64EC build tools' (Microsoft.VisualStudio.Component.VC.Tools.ARM64), " +
            "or point -Arm64CrtLibDir / LK_ARM64_CRT_LIB_DIR at a folder holding them (for example an " +
            "xwin splat's crt\lib\aarch64). -CompileOnly builds livekit_ffi.lib without linking.")
    }

    $include = @(
        "$($msvc.FullName)\include",
        "$sdkInc\ucrt", "$sdkInc\um", "$sdkInc\shared", "$sdkInc\winrt", "$sdkInc\cppwinrt"
    ) -join ';'

    $libPaths = @("$($sdk.FullName)\ucrt\arm64", "$($sdk.FullName)\um\arm64")
    if ($crtDir) { $libPaths = @($crtDir) + $libPaths }
    $rustflags = @('-C', 'target-feature=+crt-static') +
        ($libPaths | ForEach-Object { '-C'; "link-arg=/LIBPATH:$_" })

    [ordered]@{
        PATH = "$llvmBin;$env:PATH"
        INCLUDE = $include
        CC_aarch64_pc_windows_msvc = $clangCl
        CXX_aarch64_pc_windows_msvc = $clangCl
        AR_aarch64_pc_windows_msvc = $llvmLib
        CFLAGS_aarch64_pc_windows_msvc = '--target=aarch64-pc-windows-msvc'
        CXXFLAGS_aarch64_pc_windows_msvc = '--target=aarch64-pc-windows-msvc'
        CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_LINKER = $lldLink
        CARGO_ENCODED_RUSTFLAGS = $rustflags -join [char]0x1f
    }
}

foreach ($target in $Targets) {
    Write-Host "=== building livekit-ffi for $target ==="
    $saved = @{}
    if ($target -eq 'aarch64-pc-windows-msvc' -and -not (Test-NativeArm64Msvc)) {
        Write-Host "no MSVC ARM64 cross compiler; using clang-cl + lld-link from $llvmBin"
        $crossEnv = Get-Arm64CrossEnv
        foreach ($name in $crossEnv.Keys) {
            $saved[$name] = [Environment]::GetEnvironmentVariable($name)
            Set-Item "env:$name" $crossEnv[$name]
        }
    }
    try {
        if ($CompileOnly) {
            cargo rustc --release -p livekit-ffi --target $target --crate-type staticlib
            if ($LASTEXITCODE -ne 0) { throw "cargo rustc failed for $target" }
            Write-Host "compiled -> target\$target\release\livekit_ffi.lib (not linked)"
            continue
        }
        cargo build --release -p livekit-ffi --target $target
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed for $target" }
    }
    finally {
        foreach ($name in $saved.Keys) {
            if ($null -eq $saved[$name]) { Remove-Item "env:$name" -ErrorAction SilentlyContinue }
            else { Set-Item "env:$name" $saved[$name] }
        }
    }
    $rid = $rids[$target]
    $dest = Join-Path $OutDir "$rid\native"
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item "target\$target\release\livekit_ffi.dll" $dest -Force
    Write-Host "copied -> $dest\livekit_ffi.dll"
}
