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

function Get-PdbReference([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    $pe = [BitConverter]::ToInt32($bytes, 0x3c)
    if ([BitConverter]::ToUInt32($bytes, $pe) -ne 0x4550) { throw "$Path is not a PE file" }
    $sectionCount = [BitConverter]::ToUInt16($bytes, $pe + 6)
    $optionalHeaderSize = [BitConverter]::ToUInt16($bytes, $pe + 20)
    $optional = $pe + 24
    $dataDirectories = if ([BitConverter]::ToUInt16($bytes, $optional) -eq 0x20b) { $optional + 112 } else { $optional + 96 }
    $debugRva = [BitConverter]::ToUInt32($bytes, $dataDirectories + 6 * 8)
    $debugSize = [BitConverter]::ToUInt32($bytes, $dataDirectories + 6 * 8 + 4)
    if ($debugRva -eq 0) { return $null }
    $debugOffset = $null
    for ($i = 0; $i -lt $sectionCount; $i++) {
        $section = $optional + $optionalHeaderSize + 40 * $i
        $virtualSize = [BitConverter]::ToUInt32($bytes, $section + 8)
        $virtualAddress = [BitConverter]::ToUInt32($bytes, $section + 12)
        $rawSize = [BitConverter]::ToUInt32($bytes, $section + 16)
        $rawPointer = [BitConverter]::ToUInt32($bytes, $section + 20)
        if ($debugRva -ge $virtualAddress -and $debugRva -lt $virtualAddress + [Math]::Max($virtualSize, $rawSize)) {
            $debugOffset = $rawPointer + ($debugRva - $virtualAddress)
            break
        }
    }
    if ($null -eq $debugOffset) { return $null }
    for ($entry = $debugOffset; $entry -lt $debugOffset + $debugSize; $entry += 28) {
        if ([BitConverter]::ToUInt32($bytes, $entry + 12) -ne 2) { continue }
        $cv = [BitConverter]::ToUInt32($bytes, $entry + 24)
        if ([Text.Encoding]::ASCII.GetString($bytes, $cv, 4) -ne 'RSDS') { continue }
        $guid = [Guid]::new([byte[]]$bytes[($cv + 4)..($cv + 19)])
        $age = [BitConverter]::ToUInt32($bytes, $cv + 20)
        $end = [Array]::IndexOf($bytes, [byte]0, $cv + 24)
        return [pscustomobject]@{
            Guid = $guid
            Age = $age
            Path = [Text.Encoding]::UTF8.GetString($bytes, $cv + 24, $end - $cv - 24)
            DebugId = "$guid-$('{0:x}' -f $age)"
        }
    }
    return $null
}

function Write-PdbReport([string]$Pdb) {
    $pdbutil = Join-Path $llvmBin 'llvm-pdbutil.exe'
    if (-not (Test-Path $pdbutil)) {
        Write-Host "llvm-pdbutil.exe not in $llvmBin; skipping the module report"
        return
    }
    $ErrorActionPreference = 'Continue'
    $module = $null
    $obj = $null
    $modules = 0
    $withLines = 0
    $webrtcSys = @()
    $libwebrtc = 0
    $libwebrtcWithLines = 0
    foreach ($line in (& $pdbutil dump --modules $Pdb 2>$null)) {
        if ($line -match 'Mod \d+ \| `(.*)`:') { $module = $Matches[1]; $obj = $null; $modules++; continue }
        if ($line -match 'Obj: `(.*)`:') { $obj = $Matches[1]; continue }
        if ($module -and $line -match '# files: (\d+)') {
            $files = [int]$Matches[1]
            $isLibwebrtc = $obj -match '[\\/]webrtc\.lib$'
            if ($isLibwebrtc) { $libwebrtc++ }
            if ($files -gt 0) {
                $withLines++
                if ($isLibwebrtc) { $libwebrtcWithLines++ }
                if ($obj -match 'webrtc[-_]sys') { $webrtcSys += Split-Path $module -Leaf }
            }
            $module = $null
        }
    }
    $examples = ($webrtcSys | Where-Object { $_ -match 'mf_|d3d11|h264' } | Select-Object -First 3) -join ', '
    Write-Host "PDB modules: $modules, $withLines with line tables; webrtc-sys objects with line tables: $($webrtcSys.Count) (e.g. $examples); libwebrtc (webrtc.lib): $libwebrtc objects, $libwebrtcWithLines with line tables"
}

function Write-BuildInfo([string]$Dest, [string]$Rid, [string]$Target, [string]$DebugFile, [string]$DebugId) {
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
        debugFile = $DebugFile
        debugId = $DebugId
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
            [IO.File]::ReadAllText($webrtcLicense) +
            "`n# DTLN-aec (the echo model's weights, rootapp-echo-model/weights)`n$fence`n" +
            [IO.File]::ReadAllText("$PSScriptRoot\rootapp-echo-model\weights\LICENSE").TrimEnd() + "`n$fence`n"
        [IO.File]::WriteAllText("$work\LICENSE.md", $license, $utf8)
        if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
        Compress-Archive -Path "$work\livekit_ffi.dll", "$work\livekit_ffi.h", "$work\LICENSE.md" -DestinationPath $zipPath
    }
    finally {
        Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
    }
    $sha = Add-Sum -Sums (Join-Path $OutDir 'SHA256SUMS') -Path $zipPath
    Write-Host "zip -> $zipPath"
    Write-Host "sha256: $sha (recorded in $OutDir\SHA256SUMS)"
    Write-Host "desktop ffi-checksums.sha256 line: $sha  rootapp/$asset"
}

function Add-Sum([string]$Sums, [string]$Path) {
    $asset = Split-Path $Path -Leaf
    $sha = (Get-FileHash -Algorithm SHA256 $Path).Hash.ToLowerInvariant()
    $lines = @()
    if (Test-Path $Sums) {
        $lines = @([IO.File]::ReadAllLines($Sums) | Where-Object { $_ -and (($_ -split '\s+', 2)[1] -ne $asset) })
    }
    $lines += "$sha  $asset"
    [IO.File]::WriteAllText($Sums, (($lines -join "`n") + "`n"), $utf8)
    return $sha
}

# symbols-windows-<arch>.zip holds the PDB; SHA256SUMS keeps listing only the ffi-*.zip files the
# desktop pins.
function New-SymbolsZip([string]$Rid, [string]$Pdb) {
    $zipPath = Join-Path $OutDir ($assets[$Rid] -replace '^ffi-', 'symbols-')
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    Compress-Archive -Path $Pdb -DestinationPath $zipPath
    $sha = Add-Sum -Sums (Join-Path $OutDir 'SHA256SUMS-symbols') -Path $zipPath
    Write-Host "debug files zip -> $zipPath"
    Write-Host "sha256: $sha (recorded in $OutDir\SHA256SUMS-symbols)"
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
    if (-not $CompileOnly) {
        # Line tables for Rust, and /Z7 for the C/C++ that cc-rs compiles. strip = "none" makes
        # rustc link with /DEBUG and /PDBALTPATH:%_PDB% instead of /DEBUG:NONE, so the linker
        # writes livekit_ffi.pdb and the DLL names it (file name only) in a CodeView record.
        # /OPT:REF,ICF stays explicit, so the code is what a build without the PDB links.
        $targetEnv['CARGO_PROFILE_RELEASE_DEBUG'] = 'line-tables-only'
        $targetEnv['CARGO_PROFILE_RELEASE_STRIP'] = 'none'
    }
    foreach ($name in $targetEnv.Keys) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name)
        Set-Item "env:$name" $targetEnv[$name]
    }
    try {
        if ($CompileOnly) {
            cargo rustc --release -p livekit-ffi --target $target --features rustls-tls-webpki-roots --crate-type staticlib
            if ($LASTEXITCODE -ne 0) { throw "cargo rustc failed for $target" }
            Write-Host "compiled -> target\$target\release\livekit_ffi.lib (not linked)"
            continue
        }
        cargo build --release -p livekit-ffi --target $target --features rustls-tls-webpki-roots
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed for $target" }
    }
    finally {
        foreach ($name in $saved.Keys) {
            if ($null -eq $saved[$name]) { Remove-Item "env:$name" -ErrorAction SilentlyContinue }
            else { Set-Item "env:$name" $saved[$name] }
        }
    }
    $rid = $rids[$target]
    $dll = Join-Path $PSScriptRoot "target\$target\release\livekit_ffi.dll"
    $pdb = Join-Path $PSScriptRoot "target\$target\release\livekit_ffi.pdb"
    if (-not (Test-Path $pdb)) { throw "$pdb was not produced; the link did not run with /DEBUG" }
    $reference = Get-PdbReference $dll
    if (-not $reference) { throw "$dll has no CodeView (RSDS) record, so Sentry cannot match it to $pdb" }
    if ($reference.Path -ne 'livekit_ffi.pdb') { throw "$dll names its PDB '$($reference.Path)', expected livekit_ffi.pdb (rustc's /PDBALTPATH:%_PDB%)" }
    Write-Host "PDB: livekit_ffi.pdb, $((Get-Item $pdb).Length) bytes; Sentry debug id $($reference.DebugId)"
    Write-PdbReport $pdb
    $dest = Join-Path $OutDir "$rid\native"
    $debugDest = Join-Path $OutDir "$rid\debug"
    New-Item -ItemType Directory -Force $dest, $debugDest | Out-Null
    Copy-Item $dll $dest -Force
    Copy-Item $pdb $debugDest -Force
    Write-Host "copied -> $dest\livekit_ffi.dll, $debugDest\livekit_ffi.pdb"
    Write-BuildInfo -Dest $dest -Rid $rid -Target $target -DebugFile 'livekit_ffi.pdb' -DebugId $reference.DebugId
    if ($Zip) {
        New-FfiZip -Rid $rid -Dll (Join-Path $dest 'livekit_ffi.dll')
        New-SymbolsZip -Rid $rid -Pdb (Join-Path $debugDest 'livekit_ffi.pdb')
    }
}
