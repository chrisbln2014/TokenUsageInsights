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
    [string]$ShareWithServiceAccount = "token-insights-drive@demoproject-dotnet.iam.gserviceaccount.com",
    [string]$SessionEventIndexPath = "",
    [switch]$SkipSessionEvents,
    [int]$SessionEventRefreshDays = 2,
    [switch]$RefreshAllSessionEvents,
    [string[]]$SkipSessionEventAssistants = @()
)

$ErrorActionPreference = "Stop"

$DriveScope = "https://www.googleapis.com/auth/drive.file"
$CloudScope = "https://www.googleapis.com/auth/cloud-platform"
$Assistants = @("antigravity", "copilot", "codex", "claude", "cursor")
$script:AccessToken = $null
$script:AccessTokenAcquiredAt = $null
$script:AccessTokenTtlMinutes = 45
$script:SessionEventIndex = @{}

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

function Ensure-AccessToken {
    # ADC access token 壽命約 1 小時；長時間 run（大量 session event 上傳）會讓快取的
    # token 中途過期而 Drive API 回 401，整個 run abort。超過 TTL 就重新取得，避免過期。
    $expired = $false
    if ($null -ne $script:AccessTokenAcquiredAt) {
        $ageMinutes = ((Get-Date) - $script:AccessTokenAcquiredAt).TotalMinutes
        if ($ageMinutes -ge $script:AccessTokenTtlMinutes) {
            $expired = $true
        }
    }

    if ([string]::IsNullOrWhiteSpace($script:AccessToken) -or $expired) {
        $script:AccessToken = Get-GcloudAccessToken
        $script:AccessTokenAcquiredAt = Get-Date
    }

    return $script:AccessToken
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

function Invoke-DriveUploadContent {
    param(
        [string]$Token,
        [string]$Content,
        [string]$Name,
        [string]$ContentType = "application/json; charset=utf-8",
        [string]$ExistingFileId
    )

    $headers = @{
        Authorization = "Bearer $Token"
        "X-Goog-User-Project" = $ProjectId
    }
    $fileBytes = [System.Text.Encoding]::UTF8.GetBytes($Content)

    if ([string]::IsNullOrWhiteSpace($ExistingFileId)) {
        $metadata = [ordered]@{
            name = $Name
            mimeType = "application/json"
        }
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
        -ContentType $ContentType `
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
    $readerEmails = Get-SharedReaderEmails

    $escapedFileId = [System.Uri]::EscapeDataString($UploadedFileId)
    $uri = "https://www.googleapis.com/drive/v3/files/$escapedFileId/permissions?sendNotificationEmail=false&fields=id"

    foreach ($readerEmail in $readerEmails) {
        $permission = @{
            type = "user"
            role = "reader"
            emailAddress = $readerEmail
        } | ConvertTo-Json -Depth 5 -Compress

        try {
            Invoke-RestMethod -Method Post -Uri $uri -Headers $headers -Body $permission | Out-Null
        } catch {
            if ($_.Exception.Response.StatusCode.value__ -ne 409) {
                throw
            }
        }
    }
}

function Get-SharedReaderEmails {
    return @($ShareWithServiceAccount -split "[,;]" |
        ForEach-Object { $_.Trim() } |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
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

function Get-StringSha256 {
    param([string]$Value)

    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($Value)
        $hash = $sha.ComputeHash($bytes)
        return [System.BitConverter]::ToString($hash).Replace("-", "").ToLowerInvariant()
    } finally {
        $sha.Dispose()
    }
}

function Get-SessionEventFileId {
    param([object]$Existing)

    if ($null -eq $Existing -or -not $Existing.PSObject.Properties["file_id"]) {
        return ""
    }

    return [string]$Existing.file_id
}

function Convert-SessionEventRefFromIndex {
    param(
        [object]$Existing,
        [string]$FallbackFileName
    )

    $fileId = Get-SessionEventFileId $Existing
    if ([string]::IsNullOrWhiteSpace($fileId)) {
        return $null
    }

    $fileName = $FallbackFileName
    if ($Existing.PSObject.Properties["file_name"] -and
        -not [string]::IsNullOrWhiteSpace([string]$Existing.file_name)) {
        $fileName = [string]$Existing.file_name
    }

    $uploadedAt = ""
    if ($Existing.PSObject.Properties["uploaded_at"] -and
        -not [string]::IsNullOrWhiteSpace([string]$Existing.uploaded_at)) {
        $uploadedAt = [string]$Existing.uploaded_at
    }

    return [ordered]@{
        drive_file_id = $fileId
        file_name = $fileName
        content_type = "application/json"
        uploaded_at = $uploadedAt
    }
}

function Test-ShouldRefreshSessionEvent {
    param(
        [string]$SessionDate,
        [object]$Existing
    )

    if ($RefreshAllSessionEvents) {
        return $true
    }

    if ([string]::IsNullOrWhiteSpace((Get-SessionEventFileId $Existing))) {
        return $true
    }

    if ($SessionEventRefreshDays -le 0) {
        return $false
    }

    $parsedDate = [datetime]::MinValue
    if (-not [datetime]::TryParse($SessionDate, [ref]$parsedDate)) {
        return $true
    }

    $cutoff = (Get-Date).Date.AddDays(-1 * ($SessionEventRefreshDays - 1))
    return $parsedDate.Date -ge $cutoff
}

function Load-SessionEventIndex {
    param([string]$Path)

    $index = @{}
    if ([string]::IsNullOrWhiteSpace($Path) -or -not (Test-Path -LiteralPath $Path)) {
        return $index
    }

    $json = Get-Content -LiteralPath $Path -Raw
    if ([string]::IsNullOrWhiteSpace($json)) {
        return $index
    }

    $parsed = $json | ConvertFrom-Json
    foreach ($property in $parsed.PSObject.Properties) {
        $index[$property.Name] = $property.Value
    }
    return $index
}

function Save-SessionEventIndex {
    param(
        [hashtable]$Index,
        [string]$Path
    )

    if ([string]::IsNullOrWhiteSpace($Path)) {
        return
    }

    $dir = Split-Path -Parent $Path
    if (-not [string]::IsNullOrWhiteSpace($dir)) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
    }

    $ordered = [ordered]@{}
    foreach ($key in ($Index.Keys | Sort-Object)) {
        $ordered[$key] = $Index[$key]
    }
    $ordered | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $Path -Encoding utf8
}

function Upload-SessionEvent {
    param(
        [string]$Assistant,
        [string]$SessionId,
        [string]$SessionDate
    )

    if ($SkipSessionEvents -or [string]::IsNullOrWhiteSpace($SessionId)) {
        return $null
    }

    $key = "$Assistant/$SessionId"
    $fileName = "token-usage-insights-session-$Assistant-$SessionId.json"
    $existing = $script:SessionEventIndex[$key]

    if (-not (Test-ShouldRefreshSessionEvent -SessionDate $SessionDate -Existing $existing)) {
        return Convert-SessionEventRefFromIndex -Existing $existing -FallbackFileName $fileName
    }

    $raw = Invoke-TokenUsageApiRaw "/api/$Assistant/session/$SessionId"
    if ([string]::IsNullOrWhiteSpace($raw)) {
        return Convert-SessionEventRefFromIndex -Existing $existing -FallbackFileName $fileName
    }

    $hash = Get-StringSha256 $raw
    $existingFileId = $null
    if ($null -ne $existing -and $existing.PSObject.Properties["file_id"]) {
        $existingFileId = [string]$existing.file_id
    }

    $uploadedAt = (Get-Date).ToUniversalTime().ToString("o")
    $fileId = $existingFileId
    $shouldUpload = $true
    if ($null -ne $existing -and
        $existing.PSObject.Properties["sha256"] -and
        $existing.sha256 -eq $hash -and
        -not [string]::IsNullOrWhiteSpace($existingFileId)) {
        $shouldUpload = $false
        if ($existing.PSObject.Properties["uploaded_at"] -and
            -not [string]::IsNullOrWhiteSpace([string]$existing.uploaded_at)) {
            $uploadedAt = [string]$existing.uploaded_at
        }
    }

    if ($shouldUpload) {
        $uploaded = Invoke-DriveUploadContent `
            -Token (Ensure-AccessToken) `
            -Content $raw `
            -Name $fileName `
            -ExistingFileId $existingFileId
        $fileId = $uploaded.id
        Write-Host "Uploaded session event: $Assistant/$SessionId"
    }

    if ([string]::IsNullOrWhiteSpace($fileId)) {
        return $null
    }

    if ($shouldUpload) {
        Grant-DriveReader -Token (Ensure-AccessToken) -UploadedFileId $fileId
    }

    $script:SessionEventIndex[$key] = [ordered]@{
        assistant = $Assistant
        session_id = $SessionId
        file_id = $fileId
        file_name = $fileName
        sha256 = $hash
        uploaded_at = $uploadedAt
        shared_with = @(Get-SharedReaderEmails)
    }

    return [ordered]@{
        drive_file_id = $fileId
        file_name = $fileName
        content_type = "application/json"
        uploaded_at = $uploadedAt
    }
}

function Collect-SessionEventsFromApi {
    param(
        [string]$Assistant,
        [object[]]$Dates
    )

    $sessionEvents = [ordered]@{}
    if ($SkipSessionEventAssistants -contains $Assistant) {
        # 跳過此 assistant 的 session event 上傳（例如 copilot 有大量從未上傳的積壓，
        # 會讓整個 run 跑不完；核心 daily/monthly/yearly 資料仍照常匯出）。
        Write-Host "Skipping session events for $Assistant (skip list)"
        return $sessionEvents
    }
    foreach ($date in $Dates) {
        $day = Invoke-TokenUsageApi "/api/$Assistant/usage/$date"
        if ($null -eq $day) {
            continue
        }

        foreach ($session in (Get-AsArray $day "sessions")) {
            if ($null -eq $session -or -not $session.PSObject.Properties["session_id"]) {
                continue
            }

            $sessionId = [string]$session.session_id
            if ([string]::IsNullOrWhiteSpace($sessionId) -or $sessionEvents.Contains($sessionId)) {
                continue
            }

            try {
                $eventRef = Upload-SessionEvent -Assistant $Assistant -SessionId $sessionId -SessionDate ([string]$date)
            } catch {
                # 單筆 session event 上傳失敗（例如 Drive 短暫錯誤）不應中斷整個 run，
                # 否則核心 snapshot 永遠上傳不到 Drive。略過此筆、繼續。
                $status = $null
                if ($_.Exception.Response -and $_.Exception.Response.StatusCode) {
                    $status = [int]$_.Exception.Response.StatusCode.value__
                }
                if ($status -eq 401) {
                    # token 中途過期：清掉快取，讓後續 session 自動重取新 token 繼續上傳。
                    Write-Warning "Session event 遇 401（$Assistant/$sessionId），重取 token 後續筆繼續。"
                    $script:AccessToken = $null
                    $script:AccessTokenAcquiredAt = $null
                } else {
                    Write-Warning "Session event 上傳失敗（$Assistant/$sessionId），略過不中斷：$($_.Exception.Message)"
                }
                $eventRef = $null
            }
            if ($null -ne $eventRef) {
                $sessionEvents[$sessionId] = $eventRef
            }
        }
    }

    return $sessionEvents
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
            $sessionEvents = Collect-SessionEventsFromApi -Assistant $assistant -Dates $dates

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
            $writer.Write(',"session_events":')
            $writer.Write((ConvertTo-Json -InputObject $sessionEvents -Depth 20 -Compress))
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

if ([string]::IsNullOrWhiteSpace($SessionEventIndexPath)) {
    $SessionEventIndexPath = Resolve-DefaultPath "drive-session-events-index.json"
}

$script:SessionEventIndex = Load-SessionEventIndex -Path $SessionEventIndexPath

$snapshotExportPath = $SnapshotPath
$tempSnapshotPath = ""
if (-not $SkipExport) {
    $snapshotDir = Split-Path -Parent $SnapshotPath
    if (-not [string]::IsNullOrWhiteSpace($snapshotDir)) {
        New-Item -ItemType Directory -Path $snapshotDir -Force | Out-Null
    }

    $snapshotLeaf = Split-Path -Leaf $SnapshotPath
    if ([string]::IsNullOrWhiteSpace($snapshotLeaf)) {
        $snapshotLeaf = "snapshot.json"
    }
    if ([string]::IsNullOrWhiteSpace($snapshotDir)) {
        $tempSnapshotPath = "$SnapshotPath.tmp-$PID"
    } else {
        $tempSnapshotPath = Join-Path $snapshotDir "$snapshotLeaf.tmp-$PID"
    }
    $snapshotExportPath = $tempSnapshotPath
}

try {
    if ($ExportFromApi -and -not $SkipExport) {
        Export-SnapshotFromApi -OutputPath $snapshotExportPath
    } elseif (-not $SkipExport) {
        $exportArgs = @("-ExecutionPolicy", "Bypass", "-File", (Join-Path $PSScriptRoot "export-snapshot.ps1"), "-OutputPath", $snapshotExportPath)
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
            Export-SnapshotFromApi -OutputPath $snapshotExportPath
        }
    }

    if (-not $SkipExport) {
        Move-Item -LiteralPath $snapshotExportPath -Destination $SnapshotPath -Force
    }
} catch {
    if (-not [string]::IsNullOrWhiteSpace($tempSnapshotPath) -and
        (Test-Path -LiteralPath $tempSnapshotPath)) {
        Remove-Item -LiteralPath $tempSnapshotPath -Force
    }
    throw
}

if (-not $SkipSessionEvents) {
    Save-SessionEventIndex -Index $script:SessionEventIndex -Path $SessionEventIndexPath
}

if (-not (Test-Path -LiteralPath $SnapshotPath)) {
    throw "找不到 snapshot 檔案：$SnapshotPath"
}

if ([string]::IsNullOrWhiteSpace($FileId) -and (Test-Path -LiteralPath $FileIdPath)) {
    $FileId = (Get-Content -LiteralPath $FileIdPath -Raw).Trim()
}

$uploaded = $null
for ($uploadAttempt = 1; $uploadAttempt -le 2; $uploadAttempt++) {
    try {
        $accessToken = Ensure-AccessToken
        $uploaded = Invoke-DriveUpload -Token $accessToken -ContentPath $SnapshotPath -ExistingFileId $FileId
        Grant-DriveReader -Token $accessToken -UploadedFileId $uploaded.id
        break
    } catch {
        $status = $null
        if ($_.Exception.Response -and $_.Exception.Response.StatusCode) {
            $status = [int]$_.Exception.Response.StatusCode.value__
        }
        if ($status -eq 401 -and $uploadAttempt -lt 2) {
            # token 中途過期，強制重取後重試，確保核心 snapshot 一定上得去。
            Write-Warning "上傳 snapshot 時 Drive 回 401，重新取得 token 後重試。"
            $script:AccessToken = $null
            $script:AccessTokenAcquiredAt = $null
            continue
        }
        throw
    }
}

$fileIdDir = Split-Path -Parent $FileIdPath
if (-not [string]::IsNullOrWhiteSpace($fileIdDir)) {
    New-Item -ItemType Directory -Path $fileIdDir -Force | Out-Null
}
Set-Content -LiteralPath $FileIdPath -Value $uploaded.id -Encoding utf8

Write-Host "Drive snapshot uploaded."
Write-Host "File ID: $($uploaded.id)"
Write-Host "Web link: $($uploaded.webViewLink)"
Write-Host "File ID saved to: $FileIdPath"
if (-not $SkipSessionEvents) {
    Write-Host "Session event index saved to: $SessionEventIndexPath"
}
