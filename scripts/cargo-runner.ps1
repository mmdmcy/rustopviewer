param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$ExecutablePath,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ForwardedArgs
)

$ErrorActionPreference = "Stop"

$stagingRoot = Join-Path ([System.IO.Path]::GetTempPath()) "rustopviewer-cargo-run"
New-Item -ItemType Directory -Force -Path $stagingRoot | Out-Null

Get-ChildItem -Path $stagingRoot -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.LastWriteTimeUtc -lt (Get-Date).ToUniversalTime().AddDays(-2) } |
    ForEach-Object {
        try {
            Remove-Item -Path $_.FullName -Recurse -Force -ErrorAction Stop
        } catch {
            # Ignore temp cleanup failures. Old copies may still be running.
        }
    }

$runDirectory = Join-Path $stagingRoot ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null

$copiedExecutablePath = Join-Path $runDirectory ([System.IO.Path]::GetFileName($ExecutablePath))
Copy-Item -Path $ExecutablePath -Destination $copiedExecutablePath -Force

if ($ForwardedArgs.Count -gt 0) {
    & $copiedExecutablePath @ForwardedArgs
} else {
    & $copiedExecutablePath
}

if ($null -ne $LASTEXITCODE) {
    exit $LASTEXITCODE
}
