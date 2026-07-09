param(
    [string]$SnapshotPath = "",
    [string]$ExePath = "",
    [switch]$SkipExport,
    [switch]$ExportFromApi,
    [string]$ApiUrl = "http://localhost:3003",
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
$Assistants = @("antigravity", "copilot", "codex", "claude", "cursor")

function Resolve-DefaultPath {
    param([string]$Leaf)

    $base = Join-Path $env:LOCALAPPDATA "TokenUsageInsights"
    New-Item -ItemType Directory -Path $base -Force | Out-Null
    return Join-Path $base $Leaf
}

function Get-GcloudAccessToken {
    $token = & gcloud --account=$GcloudAccount auth application-default print-access-token `
        --project=$ProjectId 2>$null

    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($token)) {
        $loginCommand = "gcloud --account=$GcloudAccount auth application-default login --scopes=""$DriveScope,$CloudScope"" --project=$ProjectId"
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

    $headers = @{
        Authorization = "Bearer $Token"
        "X-Goog-User-Project" = $ProjectId
    }
    $fileBytes = [System.IO.File]::ReadAllBytes($ContentPath)

    if ([string]::IsNullOrWhiteSpace($ExistingFileId)) {
        $metadata = [ordered]@{ name = $FileName }
        if (-not [string]::IsNullOrWhiteSpace($DriveFolderId)) {
            $metadata.parents = @($DriveFolderId)
        }

        $metadataUri = "https://www.googleapis.com/drive/v3/files?fields=id,name,webViewLink,modifiedTime"
        $created = Invoke-RestMethod -Method Post `
            -Uri $metadataUri `
            -Headers $headers `
            -ContentType "application/json; charset=utf-8" `
            -Body ($metadata | ConvertTo-Json -Depth 10 -Compress)

        $ExistingFileId = $created.id
    }

    $escapedFileId = [System.Uri]::EscapeDataString($ExistingFileId)
    $updateUri = "https://www.googleapis.com/upload/drive/v3/files/${escapedFileId}?uploadType=media&fields=id,name,webViewLink,modifiedTime"
    return Invoke-RestMethod -Method Patch `
        -Uri $updateUri `
        -Headers $headers `
        -ContentType "application/json; charset=utf-8" `
        -Body $fileBytes
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
        "X-Goog-User-Project" = $ProjectId
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

function Get-AsArray {
    param(
        [object]$Value,
        [string]$PropertyName = ""
    )

    if ($null -eq $Value) {
        return @()
    }

    if (-not [string]::IsNullOrWhiteSpace($PropertyName) -and $Value.PSObject.Properties[$PropertyName]) {
        $Value = $Value.PSObject.Properties[$PropertyName].Value
        if ($null -eq $Value) {
            return @()
        }
    }

    if ($Value -is [System.Array]) {
        return @($Value)
    }
    return @($Value)
}

function Clear-TranscriptPath {
    param([object]$Value)

    if ($null -eq $Value) {
        return
    }

    if ($Value -is [System.Array]) {
        foreach ($item in $Value) {
            Clear-TranscriptPath $item
        }
        return
    }

    if ($Value -is [System.Collections.IDictionary]) {
        foreach ($key in @($Value.Keys)) {
            if ($key -eq "transcript_path") {
                $Value[$key] = ""
            } else {
                Clear-TranscriptPath $Value[$key]
            }
        }
        return
    }

    if ($Value.PSObject -and $Value.PSObject.Properties) {
        foreach ($property in $Value.PSObject.Properties) {
            if ($property.Name -eq "transcript_path") {
                $property.Value = ""
            } else {
                Clear-TranscriptPath $property.Value
            }
        }
    }
}

function Invoke-TokenUsageApi {
    param([string]$Path)

    $base = $ApiUrl.TrimEnd("/")
    $uri = "$base/$($Path.TrimStart('/'))"

    try {
        return Invoke-RestMethod -Method Get -Uri $uri -TimeoutSec 30
    } catch {
        $statusCode = $null
        if ($_.Exception.Response -and $_.Exception.Response.StatusCode) {
            $statusCode = $_.Exception.Response.StatusCode.value__
        }
        if ($statusCode -eq 400 -or $statusCode -eq 404) {
            return $null
        }
        throw
    }
}

function Invoke-TokenUsageApiRaw {
    param([string]$Path)

    $base = $ApiUrl.TrimEnd("/")
    $uri = "$base/$($Path.TrimStart('/'))"

    try {
        return (Invoke-WebRequest -UseBasicParsing -Method Get -Uri $uri -TimeoutSec 30).Content
    } catch {
        $statusCode = $null
        if ($_.Exception.Response -and $_.Exception.Response.StatusCode) {
            $statusCode = $_.Exception.Response.StatusCode.value__
        }
        if ($statusCode -eq 400 -or $statusCode -eq 404) {
            return $null
        }
        throw
    }
}

function Convert-JsonArray {
    param([object[]]$Items)
    return ConvertTo-Json -InputObject @($Items) -Compress
}

function Convert-JsonString {
    param([string]$Value)
    return ConvertTo-Json -InputObject $Value -Compress
}

function Clear-TranscriptPathJson {
    param([string]$Json)
    return [regex]::Replace($Json, '"transcript_path"\s*:\s*"(?:\\.|[^"\\])*"', '"transcript_path":""')
}

function Write-JsonMapFromApi {
    param(
        [System.IO.StreamWriter]$Writer,
        [string]$Assistant,
        [object[]]$Keys,
        [string]$PathTemplate
    )

    $Writer.Write("{")
    $first = $true
    foreach ($key in $Keys) {
        $raw = Invoke-TokenUsageApiRaw ($PathTemplate -f $Assistant, $key)
        if ([string]::IsNullOrWhiteSpace($raw)) {
            continue
        }

        if (-not $first) {
            $Writer.Write(",")
        }
        $first = $false

        $Writer.Write((Convert-JsonString $key))
        $Writer.Write(":")
        $Writer.Write((Clear-TranscriptPathJson $raw))
    }
    $Writer.Write("}")
}

function Export-SnapshotFromApi {
    param([string]$OutputPath)

    foreach ($assistant in $Assistants) {
        $dates = Get-AsArray (Invoke-TokenUsageApi "/api/$assistant/dates") "dates"
        $months = Get-AsArray (Invoke-TokenUsageApi "/api/$assistant/months") "months"
        $years = Get-AsArray (Invoke-TokenUsageApi "/api/$assistant/years") "years"
        Write-Host "Collecting ${assistant}: $($dates.Count) days, $($months.Count) months, $($years.Count) years"
    }

    $outputDir = Split-Path -Parent $OutputPath
    if (-not [string]::IsNullOrWhiteSpace($outputDir)) {
        New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
    }

    $encoding = [System.Text.UTF8Encoding]::new($false)
    $writer = [System.IO.StreamWriter]::new($OutputPath, $false, $encoding)

    try {
        $writer.Write("{")
        $writer.Write('"schema_version":1,')
        $writer.Write('"generated_at":')
        $writer.Write((Convert-JsonString (Get-Date).ToUniversalTime().ToString("o")))
        $writer.Write(',"source":"localhost-api-export","assistants":{')

        $firstAssistant = $true
        foreach ($assistant in $Assistants) {
            $dates = Get-AsArray (Invoke-TokenUsageApi "/api/$assistant/dates") "dates"
            $months = Get-AsArray (Invoke-TokenUsageApi "/api/$assistant/months") "months"
            $years = Get-AsArray (Invoke-TokenUsageApi "/api/$assistant/years") "years"

            if (-not $firstAssistant) {
                $writer.Write(",")
            }
            $firstAssistant = $false

            $writer.Write((Convert-JsonString $assistant))
            $writer.Write(":{")
            $writer.Write('"dates":')
            $writer.Write((Convert-JsonArray $dates))
            $writer.Write(',"daily":')
            Write-JsonMapFromApi -Writer $writer -Assistant $assistant -Keys $dates -PathTemplate "/api/{0}/usage/{1}"
            $writer.Write(',"months":')
            $writer.Write((Convert-JsonArray $months))
            $writer.Write(',"monthly":')
            Write-JsonMapFromApi -Writer $writer -Assistant $assistant -Keys $months -PathTemplate "/api/{0}/monthly/{1}"
            $writer.Write(',"years":')
            $writer.Write((Convert-JsonArray $years))
            $writer.Write(',"yearly":')
            Write-JsonMapFromApi -Writer $writer -Assistant $assistant -Keys $years -PathTemplate "/api/{0}/yearly/{1}"
            $writer.Write("}")
        }

        $writer.Write("}}")
    } finally {
        $writer.Dispose()
    }

    Write-Host "Snapshot exported from $ApiUrl to $OutputPath"
}

if ([string]::IsNullOrWhiteSpace($SnapshotPath)) {
    $SnapshotPath = Resolve-DefaultPath "snapshot.json"
}

if ([string]::IsNullOrWhiteSpace($FileIdPath)) {
    $FileIdPath = Resolve-DefaultPath "drive-snapshot-file-id.txt"
}

if ($ExportFromApi -and -not $SkipExport) {
    Export-SnapshotFromApi -OutputPath $SnapshotPath
} elseif (-not $SkipExport) {
    $exportArgs = @("-ExecutionPolicy", "Bypass", "-File", (Join-Path $PSScriptRoot "export-snapshot.ps1"), "-OutputPath", $SnapshotPath)
    if (-not [string]::IsNullOrWhiteSpace($ExePath)) {
        $exportArgs += @("-ExePath", $ExePath)
    }

    try {
        & pwsh @exportArgs
        if ($LASTEXITCODE -ne 0) {
            throw "snapshot 匯出失敗，exit code: $LASTEXITCODE"
        }
    } catch {
        if ([string]::IsNullOrWhiteSpace($ApiUrl)) {
            throw
        }
        Write-Warning "執行檔匯出失敗，改從 $ApiUrl 匯出 snapshot。原始錯誤：$($_.Exception.Message)"
        Export-SnapshotFromApi -OutputPath $SnapshotPath
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
