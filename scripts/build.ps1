param(
    [string]$Go = 'go',
    [string]$Zig = 'zig',
    [string]$AssetCache = (Join-Path ([IO.Path]::GetTempPath()) 'recallport-native-compare'),
    [string]$Output = 'dist/RecallPort-Granite',
    [string]$VCRedistDir
)

$ErrorActionPreference = 'Stop'
function Invoke-Go {
    & $Go @args
    if ($LASTEXITCODE -ne 0) { throw "Go failed: $args" }
}

function Get-VerifiedAsset([string]$Name, [string]$Sha256) {
    $path = Join-Path $AssetCache $Name
    if (!(Test-Path -LiteralPath $path)) { throw "Missing release asset: $path (see README)" }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash -ine $Sha256) { throw "Invalid SHA-256: $path" }
    return (Resolve-Path -LiteralPath $path).Path
}

$oldCgo = $env:CGO_ENABLED
$oldCC = $env:CC
$oldModel = $env:RECALLPORT_GRANITE_DIR
Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    $zigPath = (Get-Command $Zig -CommandType Application).Source
    $env:CC = '"' + $zigPath + '" cc'
    $env:CGO_ENABLED = '1'
    $model = Get-VerifiedAsset 'granite97/onnx_model.onnx' '68e592b160673d30250824c1116bc6ab33f70efb22b97c9e1d7ce1e69c1c9d70'
    $tokenizer = Get-VerifiedAsset 'granite97/tokenizer.json' '4f2842d568e2724370aec203652a42ac783c7937f8347a1a2cc7506d71f1582f'
    $wheel = Get-VerifiedAsset 'onnxruntime-win-amd64.whl' 'd7e0c45cc5f456ad7805e6dd256a7662736cda9c2fd17becae194eb3c2f6df72'
    $dir = (New-Item -ItemType Directory -Force -Path (Join-Path $Output 'model')).FullName
    Copy-Item -LiteralPath $model -Destination (Join-Path $dir 'model.onnx')
    Copy-Item -LiteralPath $tokenizer -Destination (Join-Path $dir 'tokenizer.json')
    if ($VCRedistDir) {
        foreach ($dll in @('msvcp140.dll','msvcp140_1.dll','vcruntime140.dll','vcruntime140_1.dll')) {
            Copy-Item -LiteralPath (Join-Path $VCRedistDir $dll) -Destination $Output
        }
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($wheel)
    try {
        foreach ($path in @('capi/onnxruntime.dll','capi/onnxruntime_providers_shared.dll','LICENSE','ThirdPartyNotices.txt','Privacy.md')) {
            $entry = $zip.GetEntry("onnxruntime/$path")
            $name = [IO.Path]::GetFileName($path)
            if ($name -eq 'LICENSE') { $name = 'onnxruntime.LICENSE' }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry,(Join-Path $dir $name),$true)
        }
    } finally { $zip.Dispose() }
    $env:RECALLPORT_GRANITE_DIR = $dir
    Invoke-Go test ./...
    Invoke-Go build -trimpath -ldflags '-s -w' -o (Join-Path $Output 'recallport.exe') ./cmd/recallport
    Get-Item -LiteralPath (Join-Path $Output 'recallport.exe') | Select-Object FullName, Length
} finally {
    $env:CGO_ENABLED = $oldCgo
    $env:CC = $oldCC
    $env:RECALLPORT_GRANITE_DIR = $oldModel
    Pop-Location
}
