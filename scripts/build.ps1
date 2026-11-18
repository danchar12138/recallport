param(
    [switch]$LexicalOnly,
    [string]$Go = 'go',
    [string]$ModelCache = (Join-Path ([IO.Path]::GetTempPath()) 'recallport-model')
)

$ErrorActionPreference = 'Stop'
function Invoke-Go {
    & $Go @args
    if ($LASTEXITCODE -ne 0) { throw "Go failed: $args" }
}

$oldCgo = $env:CGO_ENABLED
Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    $env:CGO_ENABLED = '0'
    New-Item -ItemType Directory -Force -Path 'dist' | Out-Null
    $flags = @()
    if (-not $LexicalOnly) {
        Invoke-Go run ./tools/packmodel -verified $ModelCache internal/embedding/potion.rpm
        $flags = @('-tags', 'model')
    }
    Invoke-Go test @flags ./...
    Invoke-Go build -trimpath -ldflags '-s -w' @flags -o 'dist/recallport.exe' ./cmd/recallport
    Get-Item -LiteralPath 'dist/recallport.exe' | Select-Object FullName, Length
} finally {
    $env:CGO_ENABLED = $oldCgo
    Pop-Location
}
