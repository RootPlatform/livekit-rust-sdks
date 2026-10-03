param(
    [string[]]$Targets = @('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc'),
    [string]$OutDir = "$PSScriptRoot\rootapp-dist",
    [string]$Arm64CrtLibDir = $env:LK_ARM64_CRT_LIB_DIR,
    [string]$ToolsDir = "$env:USERPROFILE\dev-tools",
    [string]$LlvmDir,
    [string]$Protoc,
    [switch]$CompileOnly,
    [switch]$Zip
)

$ErrorActionPreference = 'Stop'
if (-not $LlvmDir) { $LlvmDir = "$ToolsDir\llvm" }
if (-not $Protoc) { $Protoc = "$ToolsDir\protoc\bin\protoc.exe" }
if (-not (Test-Path $Protoc)) { throw "protoc not found at $Protoc; pass -Protoc or put it under $ToolsDir\protoc\bin" }
$env:PATH = "$env:USERPROFILE\.cargo\bin;$(Split-Path $Protoc -Parent);$env:PATH"
$env:PROTOC = $Protoc
$libclang = Get-ChildItem $LlvmDir -Recurse -Filter libclang.dll -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $libclang) { throw "libclang.dll not found under $LlvmDir; pass -LlvmDir" }
$env:LIBCLANG_PATH = $libclang.DirectoryName
$llvmBin = $libclang.DirectoryName

Set-Location $PSScriptRoot
$OutDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutDir)
$rids = @{ 'x86_64-pc-windows-msvc' = 'win-x64'; 'aarch64-pc-windows-msvc' = 'win-arm64' }
$assets = @{ 'win-x64' = 'ffi-windows-x86_64.zip'; 'win-arm64' = 'ffi-windows-arm64.zip' }
$utf8 = New-Object Text.UTF8Encoding $false
$rustChannel = (Select-String -Path "$PSScriptRoot\rust-toolchain.toml" -Pattern '^channel\s*=\s*"(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
rustup toolchain install $rustChannel --profile minimal --no-self-update
if ($LASTEXITCODE -ne 0) { throw "rustup toolchain install $rustChannel failed" }

function Get-VsInstalls {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) { throw "vswhere.exe not found; install Visual Studio or Build Tools" }
    & $vswhere -all -products * -property installationPath
}

function Get-MsvcToolDirs {
    Get-VsInstalls |
        ForEach-Object { Get-ChildItem "$_\VC\Tools\MSVC" -Directory -ErrorAction SilentlyContinue } |
        Where-Object { Test-Path "$($_.FullName)\include\vcruntime.h" } |
        Sort-Object { [version]$_.Name } -Descending
}

function Test-NativeArm64Msvc {
    foreach ($dir in Get-MsvcToolDirs) {
        if ((Test-Path "$($dir.FullName)\bin\Hostx64\arm64\cl.exe") -and
            (Test-Path "$($dir.FullName)\lib\arm64\libcmt.lib")) { return $true }
    }
    return $false
}

$arm64CrtLibs = @('libcmt.lib', 'libvcruntime.lib', 'libcpmt.lib', 'oldnames.lib', 'delayimp.lib')

function Resolve-Arm64Crt {
    if ($Arm64CrtLibDir) {
        $lib = [IO.Path]::GetFullPath($Arm64CrtLibDir).TrimEnd('\')
        $arch = Split-Path $lib -Leaf
        $libParent = Split-Path $lib -Parent
        $crtRoot = Split-Path $libParent -Parent
        if ((Split-Path $libParent -Leaf) -eq 'lib' -and (Test-Path "$crtRoot\include\vcruntime.h")) {
            $crt = @{ Lib = $lib; Include = "$crtRoot\include"; SdkInclude = $null; SdkLib = $null; Arch = $arch }
            $splat = Split-Path $crtRoot -Parent
            if ((Split-Path $crtRoot -Leaf) -eq 'crt' -and
                (Test-Path "$splat\sdk\include\um\windows.h") -and
                (Test-Path "$splat\sdk\lib\um\$arch\kernel32.lib") -and
                (Test-Path "$splat\sdk\lib\ucrt\$arch\libucrt.lib")) {
                $crt.SdkInclude = "$splat\sdk\include"
                $crt.SdkLib = "$splat\sdk\lib"
            }
            return $crt
        }
        $msvc = Get-MsvcToolDirs | Select-Object -First 1
        if (-not $msvc) { throw "no MSVC headers next to $lib (expected ..\..\include\vcruntime.h) and no MSVC toolset installed" }
        Write-Warning ("Cannot tell which MSVC version the runtime libraries in $lib belong to; compiling against " +
            "the $($msvc.Name) headers. If the versions differ the link can fail with unresolved __std_* or " +
            "vcruntime symbols. Point -Arm64CrtLibDir at <toolset>\lib\arm64 or an xwin splat's crt\lib\aarch64 " +
            "so the matching headers are used.")
        return @{ Lib = $lib; Include = "$($msvc.FullName)\include"; SdkInclude = $null; SdkLib = $null; Arch = $arch }
    }
    $toolsets = @(Get-MsvcToolDirs)
    if (-not $toolsets) { throw "no MSVC toolset with headers found" }
    $match = $toolsets | Where-Object { Test-Path "$($_.FullName)\lib\arm64\libcmt.lib" } | Select-Object -First 1
    if ($match) {
        return @{ Lib = "$($match.FullName)\lib\arm64"; Include = "$($match.FullName)\include"; SdkInclude = $null; SdkLib = $null; Arch = 'arm64' }
    }
    return @{ Lib = $null; Include = "$($toolsets[0].FullName)\include"; SdkInclude = $null; SdkLib = $null; Arch = 'arm64' }
}

function Get-Arm64CrossEnv {
    $clangCl = Join-Path $llvmBin 'clang-cl.exe'
    $lldLink = Join-Path $llvmBin 'lld-link.exe'
    $llvmLib = Join-Path $llvmBin 'llvm-lib.exe'
    foreach ($tool in @($clangCl, $lldLink, $llvmLib, (Join-Path $llvmBin 'clang.exe'))) {
        if (-not (Test-Path $tool)) { throw "$tool not found; the arm64 cross build needs clang-cl, lld-link and llvm-lib next to libclang.dll in $LlvmDir" }
    }

    $crt = Resolve-Arm64Crt
    $crtDir = $crt.Lib

    if ($crt.SdkInclude) {
        $sdkInc = $crt.SdkInclude
        $sdkLibPaths = @("$($crt.SdkLib)\ucrt\$($crt.Arch)", "$($crt.SdkLib)\um\$($crt.Arch)")
    }
    else {
        $kits = "${env:ProgramFiles(x86)}\Windows Kits\10"
        $sdk = Get-ChildItem "$kits\Lib" -Directory |
            Where-Object { Test-Path "$($_.FullName)\um\arm64\kernel32.lib" } |
            Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
        if (-not $sdk) { throw "no Windows SDK with arm64 libraries under $kits\Lib" }
        $sdkInc = "$kits\Include\$($sdk.Name)"
        $sdkLibPaths = @("$($sdk.FullName)\ucrt\arm64", "$($sdk.FullName)\um\arm64")
    }
    Write-Host "arm64 CRT headers: $($crt.Include)"
    Write-Host "arm64 CRT libs:    $(if ($crtDir) { $crtDir } else { '(none)' })"
    Write-Host "arm64 SDK headers: $sdkInc"

    $missing = if ($crtDir) { $arm64CrtLibs | Where-Object { -not (Test-Path (Join-Path $crtDir $_)) } } else { $arm64CrtLibs }
    if ($missing -and -not $CompileOnly) {
        throw ("The ARM64 MSVC runtime libraries are missing ($($missing -join ', ')" +
            $(if ($crtDir) { " in $crtDir" } else { '' }) + "). Install the Visual Studio component " +
            "'MSVC v143+ ARM64/ARM64EC build tools' (Microsoft.VisualStudio.Component.VC.Tools.ARM64), " +
            "or point -Arm64CrtLibDir / LK_ARM64_CRT_LIB_DIR at a folder holding them (for example an " +
            "xwin splat's crt\lib\aarch64). -CompileOnly builds livekit_ffi.lib without linking.")
    }

    $include = @(
        $crt.Include,
        "$sdkInc\ucrt", "$sdkInc\um", "$sdkInc\shared", "$sdkInc\winrt", "$sdkInc\cppwinrt"
    ) -join ';'

    $libPaths = $sdkLibPaths
    if ($crtDir) { $libPaths = @($crtDir) + $libPaths }
    $rustflags = @('-C', 'target-feature=+crt-static') +
        ($libPaths | ForEach-Object { '-C'; "link-arg=/LIBPATH:$_" })

    # clang-cl defines __aarch64__, which turns on libyuv's NEON/SVE/SME row functions, but
    # yuv-sys never compiles their assembly sources for Windows. MSVC (the official arm64
    # build) leaves them off, so match it.
    $arm64Flags = '--target=aarch64-pc-windows-msvc -DLIBYUV_DISABLE_NEON -DLIBYUV_DISABLE_SVE -DLIBYUV_DISABLE_SME'

    [ordered]@{
        PATH = "$llvmBin;$env:PATH"
        INCLUDE = $include
        CC_aarch64_pc_windows_msvc = $clangCl
        CXX_aarch64_pc_windows_msvc = $clangCl
        AR_aarch64_pc_windows_msvc = $llvmLib
        CFLAGS_aarch64_pc_windows_msvc = $arm64Flags
        CXXFLAGS_aarch64_pc_windows_msvc = $arm64Flags
        CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_LINKER = $lldLink
        CARGO_ENCODED_RUSTFLAGS = $rustflags -join [char]0x1f
    }
}

function Get-GitValue([string[]]$GitArgs) {
    $ErrorActionPreference = 'Continue'
    $value = & git -C $PSScriptRoot @GitArgs 2>$null
    if ($LASTEXITCODE -ne 0) { return $null }
    return $value
}

function Write-BuildInfo([string]$Dest, [string]$Rid, [string]$Target) {
    $commit = Get-GitValue @('rev-parse', 'HEAD')
    $branch = Get-GitValue @('rev-parse', '--abbrev-ref', 'HEAD')
    $dirty = [bool]($commit -and (Get-GitValue @('status', '--porcelain', '--untracked-files=no')))
    $ffiVersion = (Select-String -Path "$PSScriptRoot\livekit-ffi\Cargo.toml" -Pattern '^version\s*=\s*"(.+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value
    $info = [ordered]@{
        rid = $Rid
        target = $Target
        library = 'livekit_ffi.dll'
        librarySha256 = (Get-FileHash -Algorithm SHA256 (Join-Path $Dest 'livekit_ffi.dll')).Hash.ToLowerInvariant()
        ffiVersion = $ffiVersion
        commit = $(if ($commit) { "$commit" } else { 'unknown' })
        branch = $(if ($branch) { "$branch" } else { 'unknown' })
        dirty = $dirty
        rustc = "$(& rustc --version)"
        builtAt = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd'T'HH:mm:ss'Z'")
        buildHost = "Windows $([Environment]::OSVersion.Version) $env:PROCESSOR_ARCHITECTURE"
        minimumOs = 'Windows 10'
        hardwareVideo = 'MediaFoundation H.264'
    }
    $json = ($info | ConvertTo-Json) -replace "`r`n", "`n"
    [IO.File]::WriteAllText((Join-Path $Dest 'build-info.json'), "$json`n", $utf8)
    Write-Host "build info -> $Dest\build-info.json"
}

function New-FfiZip([string]$Rid, [string]$Dll) {
    $asset = $assets[$Rid]
    $webrtcLicense = "$PSScriptRoot\livekit-ffi\WEBRTC_LICENSE.md"
    if (-not (Test-Path $webrtcLicense)) { throw "$webrtcLicense is missing; livekit-ffi/build.rs writes it during the build" }
    $zipPath = Join-Path $OutDir $asset
    $work = Join-Path ([IO.Path]::GetTempPath()) ("rootapp-ffi-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $work | Out-Null
    try {
        Copy-Item $Dll "$work\livekit_ffi.dll"
        Copy-Item "$PSScriptRoot\livekit-ffi\include\livekit_ffi.h" "$work\livekit_ffi.h"
        $fence = '```'
        $license = "# livekit`n$fence`n" + [IO.File]::ReadAllText("$PSScriptRoot\LICENSE").TrimEnd() + "`n$fence`n" +
            [IO.File]::ReadAllText($webrtcLicense)
        [IO.File]::WriteAllText("$work\LICENSE.md", $license, $utf8)
        if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
        Compress-Archive -Path "$work\livekit_ffi.dll", "$work\livekit_ffi.h", "$work\LICENSE.md" -DestinationPath $zipPath
    }
    finally {
        Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
    }
    $sha = (Get-FileHash -Algorithm SHA256 $zipPath).Hash.ToLowerInvariant()
    $sums = Join-Path $OutDir 'SHA256SUMS'
    $lines = @()
    if (Test-Path $sums) {
        $lines = @([IO.File]::ReadAllLines($sums) | Where-Object { $_ -and (($_ -split '\s+', 2)[1] -ne $asset) })
    }
    $lines += "$sha  $asset"
    [IO.File]::WriteAllText($sums, (($lines -join "`n") + "`n"), $utf8)
    Write-Host "zip -> $zipPath"
    Write-Host "sha256: $sha (recorded in $sums)"
    Write-Host "desktop ffi-checksums.sha256 line: $sha  rootapp/$asset"
}

foreach ($target in $Targets) {
    Write-Host "=== building livekit-ffi for $target ==="
    if (-not $rids.ContainsKey($target)) { throw "unknown target $target (expected x86_64-pc-windows-msvc or aarch64-pc-windows-msvc)" }
    rustup target add --toolchain $rustChannel $target
    if ($LASTEXITCODE -ne 0) { throw "rustup target add $target failed" }
    $saved = @{}
    $targetEnv = [ordered]@{}
    if ($target -eq 'aarch64-pc-windows-msvc' -and -not (Test-NativeArm64Msvc)) {
        Write-Host "no MSVC ARM64 cross compiler; using clang-cl + lld-link from $llvmBin"
        $targetEnv = Get-Arm64CrossEnv
    }
    if ($target -eq 'x86_64-pc-windows-msvc') {
        $clangCl = Join-Path $llvmBin 'clang-cl.exe'
        if (-not (Test-Path $clangCl)) { throw "$clangCl not found; yuv-sys needs clang-cl for libyuv's SSSE3/AVX2 rows" }
        $targetEnv['LK_YUV_CC'] = $clangCl
    }
    foreach ($name in $targetEnv.Keys) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name)
        Set-Item "env:$name" $targetEnv[$name]
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
    Write-BuildInfo -Dest $dest -Rid $rid -Target $target
    if ($Zip) { New-FfiZip -Rid $rid -Dll (Join-Path $dest 'livekit_ffi.dll') }
}
