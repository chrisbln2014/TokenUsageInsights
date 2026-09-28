#requires -Modules Pester

<#
    測試 scripts/upload-drive-snapshot.ps1。

    安全規則（見 docs/superpowers/plans/2026-09-28-snapshot-events-and-memory.md 斷言 13）：
    - 一律用 dot-source 載入函式，絕不執行腳本主流程。
    - Invoke-RestMethod / Invoke-WebRequest / gcloud / pwsh 全部 mock 掉，任何沒被個別
      測試接管的呼叫都會直接丟例外，確保不會有真實網路請求或真實 Drive 上傳發生。
#>

BeforeAll {
    $script:ScriptPath = (Resolve-Path (Join-Path $PSScriptRoot "..\scripts\upload-drive-snapshot.ps1")).Path
    $script:FixturesDir = Join-Path $PSScriptRoot "fixtures"
    $script:CopilotFixtureRaw = Get-Content -Raw -LiteralPath (Join-Path $script:FixturesDir "copilot-usage-2026-09-10.json")
    $script:CopilotFixtureObj = $script:CopilotFixtureRaw | ConvertFrom-Json
}

Describe "upload-drive-snapshot.ps1：dot-source 防護（斷言 13）" {
    BeforeAll {
        $script:GuardSnapshotPath = Join-Path $TestDrive "guard-snapshot.json"
        $script:GuardFileIdPath = Join-Path $TestDrive "guard-file-id.txt"
        $script:GuardIndexPath = Join-Path $TestDrive "guard-index.json"

        # 全域保險：任何沒被個別測試接管的網路／外部程序呼叫，一律讓測試直接失敗，
        # 而不是真的發出請求或真的取得 OAuth token。
        Mock -CommandName Invoke-RestMethod -MockWith { throw "GUARD-TEST: Invoke-RestMethod 不應該被呼叫" }
        Mock -CommandName Invoke-WebRequest -MockWith { throw "GUARD-TEST: Invoke-WebRequest 不應該被呼叫" }
        Mock -CommandName gcloud -MockWith { throw "GUARD-TEST: gcloud 不應該被呼叫" }
        Mock -CommandName pwsh -MockWith { throw "GUARD-TEST: pwsh 不應該被呼叫" }

        # 用測試專用路徑 dot-source，避免任何情況下都不會碰到 %LOCALAPPDATA% 正式資料。
        . $script:ScriptPath -SnapshotPath $script:GuardSnapshotPath -FileIdPath $script:GuardFileIdPath -SessionEventIndexPath $script:GuardIndexPath
    }

    It "dot-source 後函式已定義（主流程沒有讓腳本整個中止）" {
        Get-Command Export-SnapshotFromApi -CommandType Function -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
        Get-Command Collect-SessionEventsFromApi -CommandType Function -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
        Get-Command Upload-SessionEvent -CommandType Function -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
        Get-Command Invoke-DriveUpload -CommandType Function -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
        Get-Command Invoke-DriveUploadContent -CommandType Function -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
        Get-Command Grant-DriveReader -CommandType Function -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
        Get-Command Ensure-AccessToken -CommandType Function -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
    }

    It "dot-source 期間沒有呼叫任何 Drive 或網路相關函式" {
        # gcloud／pwsh 是外部執行檔，Pester 的 Should -Invoke 呼叫次數追蹤對外部命令
        # 不可靠（實測：即使從未被呼叫，也會出現「Could not find Mock for command」
        # 的錯誤，而不是回報 0 次）。這裡改用「Mock 版的 gcloud/pwsh 一被呼叫就 throw」
        # 當保險：只要 BeforeAll 的 dot-source 沒有整個丟例外失敗，就代表兩者都沒被叫到。
        # Invoke-RestMethod／Invoke-WebRequest 是內建 cmdlet，呼叫次數追蹤可靠，直接斷言。
        Should -Invoke -CommandName Invoke-RestMethod -Times 0
        Should -Invoke -CommandName Invoke-WebRequest -Times 0
    }
}

Describe "upload-drive-snapshot.ps1：驗收條件 1（session 事件檔帶 source_kind/source_dir_key）" {
    BeforeAll {
        $script:Ac1SnapshotPath = Join-Path $TestDrive "ac1-snapshot.json"
        $script:Ac1FileIdPath = Join-Path $TestDrive "ac1-file-id.txt"
        $script:Ac1IndexPath = Join-Path $TestDrive "ac1-index.json"

        Mock -CommandName Invoke-RestMethod -MockWith { throw "AC1-TEST: Invoke-RestMethod 不應該被呼叫（沒被個別測試接管）" }
        Mock -CommandName Invoke-WebRequest -MockWith { throw "AC1-TEST: Invoke-WebRequest 不應該被呼叫（沒被個別測試接管）" }
        Mock -CommandName gcloud -MockWith { throw "AC1-TEST: gcloud 不應該被呼叫" }
        Mock -CommandName pwsh -MockWith { throw "AC1-TEST: pwsh 不應該被呼叫" }

        . $script:ScriptPath -SnapshotPath $script:Ac1SnapshotPath -FileIdPath $script:Ac1FileIdPath -SessionEventIndexPath $script:Ac1IndexPath
    }

    Context "Collect-SessionEventsFromApi 透過真實錄製的 usage 回應，把每筆 session 自己的來源參數傳給 Upload-SessionEvent" {
        BeforeEach {
            $script:CapturedCalls = [System.Collections.Generic.List[object]]::new()
            Mock -CommandName Upload-SessionEvent -MockWith {
                param($Assistant, $SessionId, $SessionDate, $SourceKind, $SourceDirKey)
                $script:CapturedCalls.Add([pscustomobject]@{
                    Assistant     = $Assistant
                    SessionId     = $SessionId
                    SessionDate   = $SessionDate
                    SourceKind    = $SourceKind
                    SourceDirKey  = $SourceDirKey
                })
                return $null
            }

            $script:Ac1DailyRawCache = @{ "2026-09-10" = $script:CopilotFixtureRaw }
        }

        It "copilot-app session 帶有 fixture 裡真實的 source_kind 與 source_dir_key" {
            Collect-SessionEventsFromApi -Assistant "copilot" -Dates @("2026-09-10") -DailyRawCache $script:Ac1DailyRawCache | Out-Null

            $expected = $script:CopilotFixtureObj.sessions | Where-Object { $_.source_kind -eq "copilot-app" } | Select-Object -First 1
            $expected | Should -Not -BeNullOrEmpty

            $call = $script:CapturedCalls | Where-Object { $_.SessionId -eq $expected.session_id }
            $call | Should -Not -BeNullOrEmpty
            $call.SourceKind | Should -Be "copilot-app"
            $call.SourceDirKey | Should -Be $expected.source_dir_key
            $call.SourceDirKey | Should -Not -BeNullOrEmpty
        }

        It "copilot-cli session（沒有 source_dir_key）只帶 source_kind，source_dir_key 維持空白" {
            Collect-SessionEventsFromApi -Assistant "copilot" -Dates @("2026-09-10") -DailyRawCache $script:Ac1DailyRawCache | Out-Null

            $expected = $script:CopilotFixtureObj.sessions | Where-Object { $_.source_kind -eq "copilot-cli" } | Select-Object -First 1
            $expected | Should -Not -BeNullOrEmpty

            $call = $script:CapturedCalls | Where-Object { $_.SessionId -eq $expected.session_id }
            $call | Should -Not -BeNullOrEmpty
            $call.SourceKind | Should -Be "copilot-cli"
            [string]::IsNullOrWhiteSpace($call.SourceDirKey) | Should -BeTrue
        }
    }

    Context "Upload-SessionEvent 本身：有值才帶查詢參數，且做 URL 編碼" {
        BeforeEach {
            $script:CapturedPaths = [System.Collections.Generic.List[string]]::new()
            Mock -CommandName Invoke-TokenUsageApiRaw -MockWith {
                param($Path)
                $script:CapturedPaths.Add($Path)
                return '{"turns":[]}'
            }
            # 這個 Context 只關心 Upload-SessionEvent 組出來的查詢路徑，不關心它會不會
            # 真的觸發 Drive 上傳，所以把 Drive 相關函式都 mock 掉，避免測試因為打到
            # 上層「不應該被呼叫」的 Invoke-RestMethod 安全網而以例外失敗收場。
            Mock -CommandName Ensure-AccessToken -MockWith { return "fake-token" }
            Mock -CommandName Invoke-DriveUploadContent -MockWith {
                return [pscustomobject]@{ id = "fake-file-id" }
            }
            Mock -CommandName Grant-DriveReader -MockWith { }
        }

        It "沒有來源欄位時，路徑跟修正前一樣，不帶查詢字串" {
            Upload-SessionEvent -Assistant "claude" -SessionId "sess-no-source" -SessionDate "2026-09-10" | Out-Null
            $script:CapturedPaths | Should -Contain "/api/claude/session/sess-no-source"
        }

        It "有 source_kind／source_dir_key 時會附加，且對特殊字元做 URL 編碼" {
            Upload-SessionEvent -Assistant "copilot" -SessionId "sess-with-source" -SessionDate "2026-09-10" `
                -SourceKind "copilot-app" -SourceDirKey '\\?\C:\Users\a b\.copilot' | Out-Null

            $expectedKind = [uri]::EscapeDataString("copilot-app")
            $expectedDirKey = [uri]::EscapeDataString('\\?\C:\Users\a b\.copilot')

            $script:CapturedPaths.Count | Should -Be 1
            $script:CapturedPaths[0] | Should -Match ([regex]::Escape("/api/copilot/session/sess-with-source?"))
            $script:CapturedPaths[0] | Should -Match ([regex]::Escape("source_kind=$expectedKind"))
            $script:CapturedPaths[0] | Should -Match ([regex]::Escape("source_dir_key=$expectedDirKey"))
            # 沒編碼過的原始反斜線／空白不應該直接出現在查詢字串裡。
            $script:CapturedPaths[0] | Should -Not -Match ([regex]::Escape('a b\.copilot'))
        }

        It "SessionId 本身帶特殊字元時，URL 路徑段落也要做 URL 編碼（不只是查詢參數）" {
            # 目前實際資料裡的 session id 只含英數字/底線/連字號不會出事，但這裡刻意用
            # 含 / # 空白 & 的字串，模擬路徑被誤解析的防禦性缺口（審查員發現 1）。
            $specialSessionId = 'sess/with #chars &weird id'
            Upload-SessionEvent -Assistant "claude" -SessionId $specialSessionId -SessionDate "2026-09-10" | Out-Null

            $expectedSessionId = [uri]::EscapeDataString($specialSessionId)

            $script:CapturedPaths.Count | Should -Be 1
            $script:CapturedPaths[0] | Should -Match ([regex]::Escape("/api/claude/session/$expectedSessionId"))
            # 未編碼的原始特殊字元不應該直接出現在路徑裡，否則路徑會被切壞。
            $script:CapturedPaths[0] | Should -Not -Match ([regex]::Escape($specialSessionId))
        }
    }
}

Describe "upload-drive-snapshot.ps1：驗收條件 2（同一天的 usage 只查一次，daily 與 session_events 一致）" {
    BeforeAll {
        $script:Ac2SnapshotPath = Join-Path $TestDrive "ac2-snapshot.json"
        $script:Ac2FileIdPath = Join-Path $TestDrive "ac2-file-id.txt"
        $script:Ac2IndexPath = Join-Path $TestDrive "ac2-index.json"

        Mock -CommandName Invoke-RestMethod -MockWith { throw "AC2-TEST: Invoke-RestMethod 不應該被呼叫（沒被個別測試接管）" }
        Mock -CommandName Invoke-WebRequest -MockWith { throw "AC2-TEST: Invoke-WebRequest 不應該被呼叫（沒被個別測試接管）" }
        Mock -CommandName gcloud -MockWith { throw "AC2-TEST: gcloud 不應該被呼叫" }
        Mock -CommandName pwsh -MockWith { throw "AC2-TEST: pwsh 不應該被呼叫" }

        . $script:ScriptPath -SnapshotPath $script:Ac2SnapshotPath -FileIdPath $script:Ac2FileIdPath -SessionEventIndexPath $script:Ac2IndexPath
    }

    Context "usage/2026-09-10 中途新開一個 session（模擬 30 分鐘後才會出現的第二次查詢結果）" {
        BeforeEach {
            # 第二次呼叫的內容：在 fixture 既有 sessions 之外，多一個新 session，
            # 模擬「執行到一半，新開了一個 session」。這是測試自己組出來的 mock 行為，
            # 不是改寫 fixture 檔案本身（fixture 檔案原封不動）。
            $extraSession = $script:CopilotFixtureObj.sessions[0] | Select-Object *
            $extraSession.session_id = "midrun-new-session-0001"
            $extraSession.source_kind = "copilot-cli"
            $extraSession.source_dir_key = $null

            $secondCallObj = $script:CopilotFixtureObj | Select-Object *
            $secondCallObj.sessions = @($script:CopilotFixtureObj.sessions) + $extraSession
            $script:SecondCallRaw = $secondCallObj | ConvertTo-Json -Depth 20 -Compress

            $script:UsageDateHits = 0
            $script:CapturedUsageDatePaths = [System.Collections.Generic.List[string]]::new()

            Mock -CommandName Upload-SessionEvent -MockWith {
                param($Assistant, $SessionId, $SessionDate, $SourceKind, $SourceDirKey)
                return [ordered]@{
                    drive_file_id = "fake-$SessionId"
                    file_name     = "fake-$SessionId.json"
                    content_type  = "application/json"
                    uploaded_at   = "2026-09-10T00:00:00Z"
                }
            }

            Mock -CommandName Invoke-TokenUsageApi -MockWith {
                param($Path)
                if ($Path -eq "/api/copilot/dates") { return [pscustomobject]@{ dates = @("2026-09-10") } }
                if ($Path -match "^/api/[^/]+/dates$") { return [pscustomobject]@{ dates = @() } }
                if ($Path -match "/months$") { return [pscustomobject]@{ months = @() } }
                if ($Path -match "/years$") { return [pscustomobject]@{ years = @() } }
                if ($Path -eq "/api/copilot/usage/2026-09-10") {
                    $script:UsageDateHits++
                    $script:CapturedUsageDatePaths.Add($Path)
                    if ($script:UsageDateHits -eq 1) {
                        return ($script:CopilotFixtureRaw | ConvertFrom-Json)
                    }
                    return ($script:SecondCallRaw | ConvertFrom-Json)
                }
                return $null
            }

            Mock -CommandName Invoke-TokenUsageApiRaw -MockWith {
                param($Path)
                if ($Path -eq "/api/copilot/usage/2026-09-10") {
                    $script:UsageDateHits++
                    $script:CapturedUsageDatePaths.Add($Path)
                    if ($script:UsageDateHits -eq 1) {
                        return $script:CopilotFixtureRaw
                    }
                    return $script:SecondCallRaw
                }
                return $null
            }
        }

        It "/api/copilot/usage/2026-09-10 這個日期的 usage 只被查一次（不分用哪個函式查）" {
            $outputPath = Join-Path $TestDrive "ac2-export.json"
            Export-SnapshotFromApi -OutputPath $outputPath

            $script:CapturedUsageDatePaths.Count | Should -Be 1
        }

        It "輸出 snapshot 裡 daily 與 session_events 的 session 完全一致" {
            $outputPath = Join-Path $TestDrive "ac2-export-2.json"
            $script:UsageDateHits = 0
            $script:CapturedUsageDatePaths.Clear()

            Export-SnapshotFromApi -OutputPath $outputPath

            $snapshot = Get-Content -Raw -LiteralPath $outputPath | ConvertFrom-Json
            $copilotBlock = $snapshot.assistants.copilot
            $copilotBlock | Should -Not -BeNullOrEmpty

            $dailySessionIds = @($copilotBlock.daily.'2026-09-10'.sessions | ForEach-Object { $_.session_id }) | Sort-Object -Unique
            $eventSessionIds = @($copilotBlock.session_events.PSObject.Properties.Name) | Sort-Object -Unique

            # 只查一次的話，daily 應該剛好是 fixture 原本的 57 筆，
            # 不會混進「第二次查詢」才有的 midrun-new-session-0001。
            $dailySessionIds.Count | Should -Be $script:CopilotFixtureObj.sessions.Count
            $dailySessionIds | Should -Be $eventSessionIds
            $dailySessionIds | Should -Not -Contain "midrun-new-session-0001"
        }
    }
}

Describe "upload-drive-snapshot.ps1：Get-DailyRawCache 對重複日期不重複查詢" {
    BeforeAll {
        $script:Ac3SnapshotPath = Join-Path $TestDrive "ac3-snapshot.json"
        $script:Ac3FileIdPath = Join-Path $TestDrive "ac3-file-id.txt"
        $script:Ac3IndexPath = Join-Path $TestDrive "ac3-index.json"

        Mock -CommandName Invoke-RestMethod -MockWith { throw "AC3-TEST: Invoke-RestMethod 不應該被呼叫（沒被個別測試接管）" }
        Mock -CommandName Invoke-WebRequest -MockWith { throw "AC3-TEST: Invoke-WebRequest 不應該被呼叫（沒被個別測試接管）" }
        Mock -CommandName gcloud -MockWith { throw "AC3-TEST: gcloud 不應該被呼叫" }
        Mock -CommandName pwsh -MockWith { throw "AC3-TEST: pwsh 不應該被呼叫" }

        . $script:ScriptPath -SnapshotPath $script:Ac3SnapshotPath -FileIdPath $script:Ac3FileIdPath -SessionEventIndexPath $script:Ac3IndexPath
    }

    Context "傳入的日期清單本身含重複值（例如 /api/{assistant}/dates 回傳裡混進重複日期）" {
        BeforeEach {
            $script:CapturedDateCalls = [System.Collections.Generic.List[string]]::new()
            Mock -CommandName Invoke-TokenUsageApiRaw -MockWith {
                param($Path)
                $script:CapturedDateCalls.Add($Path)
                return '{"sessions":[]}'
            }
        }

        It "同一個日期只被實際查詢一次，輸出快取裡每個日期也只有一筆（審查員發現 2）" {
            $dates = @("2026-09-10", "2026-09-11", "2026-09-10")
            $cache = Get-DailyRawCache -Assistant "claude" -Dates $dates

            $hitsFor0910 = @($script:CapturedDateCalls | Where-Object { $_ -eq "/api/claude/usage/2026-09-10" })
            $hitsFor0910.Count | Should -Be 1
            $script:CapturedDateCalls.Count | Should -Be 2

            $cache.Keys.Count | Should -Be 2
            $cache["2026-09-10"] | Should -Not -BeNullOrEmpty
            $cache["2026-09-11"] | Should -Not -BeNullOrEmpty
        }
    }
}

Describe "upload-drive-snapshot.ps1：Get-UniqueOrdered 去重後維持原始出現順序" {
    BeforeAll {
        $script:Ac5SnapshotPath = Join-Path $TestDrive "ac5-snapshot.json"
        $script:Ac5FileIdPath = Join-Path $TestDrive "ac5-file-id.txt"
        $script:Ac5IndexPath = Join-Path $TestDrive "ac5-index.json"

        Mock -CommandName Invoke-RestMethod -MockWith { throw "AC5-TEST: Invoke-RestMethod 不應該被呼叫" }
        Mock -CommandName Invoke-WebRequest -MockWith { throw "AC5-TEST: Invoke-WebRequest 不應該被呼叫" }
        Mock -CommandName gcloud -MockWith { throw "AC5-TEST: gcloud 不應該被呼叫" }
        Mock -CommandName pwsh -MockWith { throw "AC5-TEST: pwsh 不應該被呼叫" }

        . $script:ScriptPath -SnapshotPath $script:Ac5SnapshotPath -FileIdPath $script:Ac5FileIdPath -SessionEventIndexPath $script:Ac5IndexPath
    }

    It "輸入順序非字典序時，去重後仍照原始出現順序排列（不能用 Select-Object -Unique，那會重新排序）" {
        # 刻意用「後面的日期比前面小」的輸入：如果去重邏輯內部誤用了
        # Select-Object -Unique（會重新排序成字典序 09-10, 09-11），
        # 這條測試會抓到，因為預期結果是保留原始出現順序 09-11, 09-10。
        $result = Get-UniqueOrdered -Values @("2026-09-11", "2026-09-10", "2026-09-11")
        @($result) | Should -Be @("2026-09-11", "2026-09-10")
    }
}

Describe "upload-drive-snapshot.ps1：Export-SnapshotFromApi 對日期清單本身的重複值做去重（codex 總審發現：只防重打 API，沒防輸出重複 key）" {
    BeforeAll {
        $script:Ac4SnapshotPath = Join-Path $TestDrive "ac4-snapshot.json"
        $script:Ac4FileIdPath = Join-Path $TestDrive "ac4-file-id.txt"
        $script:Ac4IndexPath = Join-Path $TestDrive "ac4-index.json"

        Mock -CommandName Invoke-RestMethod -MockWith { throw "AC4-TEST: Invoke-RestMethod 不應該被呼叫（沒被個別測試接管）" }
        Mock -CommandName Invoke-WebRequest -MockWith { throw "AC4-TEST: Invoke-WebRequest 不應該被呼叫（沒被個別測試接管）" }
        Mock -CommandName gcloud -MockWith { throw "AC4-TEST: gcloud 不應該被呼叫" }
        Mock -CommandName pwsh -MockWith { throw "AC4-TEST: pwsh 不應該被呼叫" }

        . $script:ScriptPath -SnapshotPath $script:Ac4SnapshotPath -FileIdPath $script:Ac4FileIdPath -SessionEventIndexPath $script:Ac4IndexPath
    }

    Context "/api/claude/dates 回傳的日期清單本身含重複值（例如 @('2026-09-10','2026-09-11','2026-09-10')）" {
        BeforeEach {
            Mock -CommandName Upload-SessionEvent -MockWith {
                param($Assistant, $SessionId, $SessionDate, $SourceKind, $SourceDirKey)
                return $null
            }

            Mock -CommandName Invoke-TokenUsageApi -MockWith {
                param($Path)
                if ($Path -eq "/api/claude/dates") { return [pscustomobject]@{ dates = @("2026-09-10", "2026-09-11", "2026-09-10") } }
                if ($Path -match "^/api/[^/]+/dates$") { return [pscustomobject]@{ dates = @() } }
                if ($Path -match "/months$") { return [pscustomobject]@{ months = @() } }
                if ($Path -match "/years$") { return [pscustomobject]@{ years = @() } }
                return $null
            }

            Mock -CommandName Invoke-TokenUsageApiRaw -MockWith {
                param($Path)
                if ($Path -eq "/api/claude/usage/2026-09-10") { return '{"sessions":[]}' }
                if ($Path -eq "/api/claude/usage/2026-09-11") { return '{"sessions":[]}' }
                return $null
            }
        }

        It "輸出的 daily JSON 裡 2026-09-10 這個 key 只出現一次，dates 陣列也已去重，且整份 JSON 能被正常解析（重現 codex 審查發現的重複 key 問題）" {
            $outputPath = Join-Path $TestDrive "ac4-export.json"
            Export-SnapshotFromApi -OutputPath $outputPath

            $rawJson = Get-Content -Raw -LiteralPath $outputPath

            # 直接數原始文字裡 daily 物件的 key 出現次數：duplicate key 一旦被
            # ConvertFrom-Json 解析成 PSObject，不同 PowerShell 版本對重複屬性的
            # 處理方式不保證一致，只有從原始文字比對才能可靠證明「輸出本身」
            # 有沒有重複 key。
            $dailyKeyMatches = [regex]::Matches($rawJson, [regex]::Escape('"2026-09-10":{'))
            $dailyKeyMatches.Count | Should -Be 1

            { $rawJson | ConvertFrom-Json } | Should -Not -Throw

            $snapshot = $rawJson | ConvertFrom-Json
            @($snapshot.assistants.claude.dates) | Should -Be @("2026-09-10", "2026-09-11")
            @($snapshot.assistants.claude.dates).Count | Should -Be 2
        }
    }
}
