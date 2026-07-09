param(
    [string]$OutputPath = "",
    [string]$ExePath = ""
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $driveCandidates = @(
        (Join-Path $env:USERPROFILE "Google Drive\TokenUsageInsights\snapshot.json"),
        "G:\My Drive\TokenUsageInsights\snapshot.json"
    )
    $OutputPath = $driveCandidates[0]
}

if ([string]::IsNullOrWhiteSpace($ExePath)) {
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA "TokenUsageInsights\token-usage-insights.exe"),
        (Join-Path $PSScriptRoot "..\target\release\token-usage-insights.exe"),
        "token-usage-insights.exe"
    )
    foreach ($candidate in $candidates) {
        if (Get-Command $candidate -ErrorAction SilentlyContinue) {
            $ExePath = (Get-Command $candidate).Source
            break
        }
        if (Test-Path -LiteralPath $candidate) {
            $ExePath = (Resolve-Path -LiteralPath $candidate).Path
            break
        }
    }
}

if ([string]::IsNullOrWhiteSpace($ExePath)) {
    throw "找不到 token-usage-insights.exe。請用 -ExePath 指定新版執行檔。"
}

$outputDir = Split-Path -Parent $OutputPath
if (-not [string]::IsNullOrWhiteSpace($outputDir)) {
    New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
}

& $ExePath --export-snapshot $OutputPath
if ($LASTEXITCODE -ne 0) {
    throw "snapshot 匯出失敗，exit code: $LASTEXITCODE"
}

Write-Host "Snapshot exported to $OutputPath"
