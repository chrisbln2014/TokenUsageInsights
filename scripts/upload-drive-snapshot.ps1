param(
    [string]$SnapshotPath = "",
    [string]$ExePath = "",
    [switch]$SkipExport,
    [string]$GcloudAccount = "chris@berlin.com.tw",
    [string]$ProjectId = "tokenusage-chris-20260709",
    [string]$FileName = "token-usage-insights-snapshot.json",
    [string]$FileId = "",
    [string]$FileIdPath = "",
    [string]$DriveFolderId = "",
    [string]$ShareWithServiceAccount = "token-insights-run@tokenusage-chris-20260709.iam.gserviceaccount.com"
)

$ErrorActionPreference = "Stop"

$DriveScope = "https://www.googleapis.com/auth/drive.file"
$CloudScope = "https://www.googleapis.com/auth/cloud-platform"

function Resolve-DefaultPath {
    param([string]$Leaf)

    $base = Join-Path $env:LOCALAPPDATA "TokenUsageInsights"
    New-Item -ItemType Directory -Path $base -Force | Out-Null
    return Join-Path $base $Leaf
}

function Get-GcloudAccessToken {
    $token = & gcloud --account=$GcloudAccount auth application-default print-access-token `
        --scopes=$DriveScope,$CloudScope `
        --project=$ProjectId 2>$null

    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($token)) {
        $loginCommand = "gcloud --account=$GcloudAccount auth application-default login $GcloudAccount --scopes=$DriveScope,$CloudScope --project=$ProjectId"
        throw "無法取得 Google Drive OAuth token。請先執行：$loginCommand"
    }

    return ($token | Select-Object -First 1).Trim()
}

function New-MultipartBody {
    param(
        [object]$Metadata,
        [string]$ContentPath,
        [string]$Boundary
    )

    $stream = [System.IO.MemoryStream]::new()

    function Add-Utf8 {
        param(
            [System.IO.Stream]$Target,
            [string]$Text
        )
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
        $Target.Write($bytes, 0, $bytes.Length)
    }

    $metadataJson = $Metadata | ConvertTo-Json -Depth 10 -Compress
    $fileBytes = [System.IO.File]::ReadAllBytes($ContentPath)

    Add-Utf8 $stream "--$Boundary`r`n"
    Add-Utf8 $stream "Content-Type: application/json; charset=UTF-8`r`n`r`n"
    Add-Utf8 $stream "$metadataJson`r`n"
    Add-Utf8 $stream "--$Boundary`r`n"
    Add-Utf8 $stream "Content-Type: application/json`r`n`r`n"
    $stream.Write($fileBytes, 0, $fileBytes.Length)
    Add-Utf8 $stream "`r`n--$Boundary--`r`n"

    return $stream.ToArray()
}

function Invoke-DriveUpload {
    param(
        [string]$Token,
        [string]$ContentPath,
        [string]$ExistingFileId
    )

    $boundary = "token_usage_snapshot_" + [Guid]::NewGuid().ToString("N")
    $metadata = [ordered]@{ name = $FileName }
    if (-not [string]::IsNullOrWhiteSpace($DriveFolderId) -and [string]::IsNullOrWhiteSpace($ExistingFileId)) {
        $metadata.parents = @($DriveFolderId)
    }

    $body = New-MultipartBody -Metadata $metadata -ContentPath $ContentPath -Boundary $boundary
    $headers = @{ Authorization = "Bearer $Token" }
    $contentType = "multipart/related; boundary=$boundary"

    if ([string]::IsNullOrWhiteSpace($ExistingFileId)) {
        $uri = "https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart&fields=id,name,webViewLink,modifiedTime"
        return Invoke-RestMethod -Method Post -Uri $uri -Headers $headers -ContentType $contentType -Body $body
    }

    $escapedFileId = [System.Uri]::EscapeDataString($ExistingFileId)
    $updateUri = "https://www.googleapis.com/upload/drive/v3/files/$escapedFileId?uploadType=multipart&fields=id,name,webViewLink,modifiedTime"
    return Invoke-RestMethod -Method Patch -Uri $updateUri -Headers $headers -ContentType $contentType -Body $body
}

function Grant-DriveReader {
    param(
        [string]$Token,
        [string]$UploadedFileId
    )

    if ([string]::IsNullOrWhiteSpace($ShareWithServiceAccount)) {
        return
    }

    $headers = @{
        Authorization = "Bearer $Token"
        "Content-Type" = "application/json"
    }
    $permission = @{
        type = "user"
        role = "reader"
        emailAddress = $ShareWithServiceAccount
    } | ConvertTo-Json -Depth 5 -Compress

    $escapedFileId = [System.Uri]::EscapeDataString($UploadedFileId)
    $uri = "https://www.googleapis.com/drive/v3/files/$escapedFileId/permissions?sendNotificationEmail=false&fields=id"

    try {
        Invoke-RestMethod -Method Post -Uri $uri -Headers $headers -Body $permission | Out-Null
    } catch {
        if ($_.Exception.Response.StatusCode.value__ -ne 409) {
            throw
        }
    }
}

if ([string]::IsNullOrWhiteSpace($SnapshotPath)) {
    $SnapshotPath = Resolve-DefaultPath "snapshot.json"
}

if ([string]::IsNullOrWhiteSpace($FileIdPath)) {
    $FileIdPath = Resolve-DefaultPath "drive-snapshot-file-id.txt"
}

if (-not $SkipExport) {
    $exportArgs = @("-ExecutionPolicy", "Bypass", "-File", (Join-Path $PSScriptRoot "export-snapshot.ps1"), "-OutputPath", $SnapshotPath)
    if (-not [string]::IsNullOrWhiteSpace($ExePath)) {
        $exportArgs += @("-ExePath", $ExePath)
    }

    & pwsh @exportArgs
    if ($LASTEXITCODE -ne 0) {
        throw "snapshot 匯出失敗，exit code: $LASTEXITCODE"
    }
}

if (-not (Test-Path -LiteralPath $SnapshotPath)) {
    throw "找不到 snapshot 檔案：$SnapshotPath"
}

if ([string]::IsNullOrWhiteSpace($FileId) -and (Test-Path -LiteralPath $FileIdPath)) {
    $FileId = (Get-Content -LiteralPath $FileIdPath -Raw).Trim()
}

$accessToken = Get-GcloudAccessToken
$uploaded = Invoke-DriveUpload -Token $accessToken -ContentPath $SnapshotPath -ExistingFileId $FileId
Grant-DriveReader -Token $accessToken -UploadedFileId $uploaded.id

$fileIdDir = Split-Path -Parent $FileIdPath
if (-not [string]::IsNullOrWhiteSpace($fileIdDir)) {
    New-Item -ItemType Directory -Path $fileIdDir -Force | Out-Null
}
Set-Content -LiteralPath $FileIdPath -Value $uploaded.id -Encoding utf8

Write-Host "Drive snapshot uploaded."
Write-Host "File ID: $($uploaded.id)"
Write-Host "Web link: $($uploaded.webViewLink)"
Write-Host "File ID saved to: $FileIdPath"
