param(
    [string]$Cargo = 'cargo',
    [string]$AssetCache = (Join-Path $env:TEMP 'recallport-native-compare'),
    [string]$Output = 'dist/RecallPort',
    [string]$VCRedistDir,
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'
function Invoke-Cargo {
    & $Cargo @args
    if ($LASTEXITCODE -ne 0) { throw "Cargo failed: $args" }
}
function Get-VerifiedAsset([string]$Name, [string]$Sha256) {
    $path = Join-Path $AssetCache $Name
    if (!(Test-Path -LiteralPath $path)) { throw "Missing asset: $path (see README)" }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash -ine $Sha256) { throw "Invalid SHA-256: $path" }
    return (Resolve-Path -LiteralPath $path).Path
}
function Get-VCRedist([string]$Folder) {
    if ($Folder) { return (Resolve-Path -LiteralPath $Folder).Path }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    if (Test-Path -LiteralPath $vswhere) {
        $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($vs) {
            $crt = Get-ChildItem -Directory -Path (Join-Path $vs 'VC/Redist/MSVC/*/x64/Microsoft.VC*.CRT') | Sort-Object FullName -Descending | Select-Object -First 1
            if ($crt) { return $crt.FullName }
        }
    }
    throw 'Supply -VCRedistDir for the Microsoft x64 app-local CRT DLLs.'
}

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    $model = Get-VerifiedAsset 'granite97/onnx_model.onnx' '68e592b160673d30250824c1116bc6ab33f70efb22b97c9e1d7ce1e69c1c9d70'
    $tokenizer = Get-VerifiedAsset 'granite97/tokenizer.json' '4f2842d568e2724370aec203652a42ac783c7937f8347a1a2cc7506d71f1582f'
    $wheel = Get-VerifiedAsset 'onnxruntime-win-amd64.whl' 'd7e0c45cc5f456ad7805e6dd256a7662736cda9c2fd17becae194eb3c2f6df72'
    $crt = Get-VCRedist $VCRedistDir
    $dlls = @('msvcp140.dll','msvcp140_1.dll','vcruntime140.dll','vcruntime140_1.dll')
    foreach ($dll in $dlls) { if (!(Test-Path -LiteralPath (Join-Path $crt $dll) -PathType Leaf)) { throw "Missing x64 CRT DLL: $dll" } }
    $metadata = & $Cargo metadata --no-deps --locked --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed' }
    $hostInfo = (& $Cargo -vV) -join "`n"
    if ($hostInfo -notmatch '(?m)^host: x86_64-pc-windows-msvc\s*$') { throw 'Use an x64 Windows MSVC Cargo toolchain.' }
    if (!$SkipTests) { Invoke-Cargo test --locked }
    Invoke-Cargo build --release --locked
    $exe = Join-Path $metadata.target_directory 'release/recallport.exe'

    $dir = (New-Item -ItemType Directory -Force -Path (Join-Path $Output 'model')).FullName
    Copy-Item -LiteralPath $model -Destination (Join-Path $dir 'model.onnx')
    Copy-Item -LiteralPath $tokenizer -Destination (Join-Path $dir 'tokenizer.json')
    foreach ($dll in $dlls) { Copy-Item -LiteralPath (Join-Path $crt $dll) -Destination $Output }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($wheel)
    try {
        foreach ($path in @('capi/onnxruntime.dll','capi/onnxruntime_providers_shared.dll','LICENSE','ThirdPartyNotices.txt','Privacy.md')) {
            $entry = $zip.GetEntry("onnxruntime/$path")
            if (!$entry) { throw "Missing ONNX Runtime wheel entry: $path" }
            $name = [IO.Path]::GetFileName($path)
            if ($name -eq 'LICENSE') { $name = 'onnxruntime.LICENSE' }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry,(Join-Path $dir $name),$true)
        }
    } finally { $zip.Dispose() }
    $licenses = (New-Item -ItemType Directory -Force -Path (Join-Path $Output 'licenses')).FullName
    Get-ChildItem -LiteralPath 'web/vendor' -Filter '*.LICENSE' | Copy-Item -Destination $licenses
    Copy-Item -LiteralPath $exe -Destination (Join-Path $Output 'RecallPort.exe')
    foreach ($doc in @('README.md','README.en.md','LICENSE','CHANGELOG.md')) { if (Test-Path -LiteralPath $doc) { Copy-Item -LiteralPath $doc -Destination $Output } }
    Get-Item -LiteralPath (Join-Path $Output 'RecallPort.exe') | Select-Object FullName, Length
} finally { Pop-Location }
