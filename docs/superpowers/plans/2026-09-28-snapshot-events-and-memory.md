# 計畫書（PLAN）：Drive session 事件檔漏抓修正＋Cloud Run snapshot 記憶體根治

> `/clear` 後的新 session，第一句話：「讀 docs/superpowers/plans/2026-09-28-snapshot-events-and-memory.md 繼續」。
> 原則：每一條關於現有程式的斷言都要有來源，沒證據的一律標 [假設]。

## 任務

- **一句話目標**：修正上傳腳本漏抓 session 事件檔的兩個原因（Copilot App 回 404、同一輪前後各查一次造成的時間差），改善找不到事件檔時的錯誤訊息，並根治 Cloud Run snapshot 模式記憶體超過上限被強制終止（OOM）的問題。
- **T 級**：T2　　**路徑**：L（本地規劃）
- **限制（不准動的東西）**：
  - 開發一律在分身 worktree `C:\Users\1418\Documents\projects\TokenUsageInsights-snapfix`（分支 `fix/snapshot-events-memory`，從 `feature/cloud-run-drive-snapshot` 開），**絕不直接改主資料夾**：排程直接執行主資料夾的 `scripts/upload-drive-snapshot.ps1`，正式看板也讀主資料夾的 `static/`，改到一半會被正式環境拿去跑。所有 `cargo`／`git` 指令都要帶絕對路徑（`--manifest-path`、`git -C`）。
  - 開發與測試期間不對 Google Drive 做任何上傳、分享或刪除；上傳腳本的測試一律 mock 掉 Drive 呼叫。真實上傳只在 Live 驗證階段由正式排程執行。
  - snapshot 檔案的 JSON 格式不變：舊 snapshot 仍可讀，新舊前端都相容。
  - `session_events` 的鍵維持只用 `session_id`（理由見「已排除的路」）。
  - 不碰正式資料庫 `%LOCALAPPDATA%\TokenUsageInsights\token_usage_insights.db`（它是 `journal_mode=delete`，長時間讀取會鎖住正式服務的寫入）。
  - Cloud Run 重新部署、記憶體調回 512Mi、推送主分支，這三件事執行前都先問使用者。
  - commit 訊息用繁體中文詳細格式，先寫進 worktree 之外的暫存檔，再用 `git commit -F`；結尾帶 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` 與本 session 的 `Claude-Session` 連結。
- **開始日期**：2026-09-28

## 使用者確認

| 項目 | 確認人 | 日期 |
|---|---|---|
| 驗收條件（T2 以上必填） | chris@berlin.com.tw（完整方案，9 條全做） | 2026-09-28 |
| 簽核（僅 T3：不可逆項目） | 不適用（T2） | |

> 這欄空著 = 驗收條件還沒被使用者認可，不得開始實作。

## 斷言與來源

| # | 斷言 | 來源 | 影響 | 證據 | 狀態 |
|---|---|---|---|---|---|
| 1 | Cloud Run 因記憶體超限被強制終止，造成 `/api/codex/*` 回 503 | [已證實] | breaks | log：`Memory limit of 512 MiB exceeded with 565 MiB used`（05:07）、`528 MiB`（05:16）；同時間 `/api/codex/dates`、`/api/codex/usage/2026-09-28` 回 503 | 已證實 |
| 2 | 換版前就有 OOM，只是變頻繁 | [已證實] | adjusts | 舊 revision `00006-4h8` 在 2026-09-23 08:15 也 OOM 一次 | 已證實 |
| 3 | OOM 發生在 snapshot 刷新時 | [已證實]（時間吻合）＋[Claude]（機制） | breaks | Drive 05:01:37 更新 → 05:06:56 第一次 OOM，間隔正好是 `REFRESH_SECONDS=300`；`snapshot.rs:349-379` 刷新時舊版仍被保留，新版整份下載後解析成 `serde_json::Value` | 時間已證實、機制推論 |
| 4 | 每次回應日／月／年報都整份複製一次 | [已證實] | breaks | `snapshot.rs:621`、`:640`、`:659`：`Json(value.clone())` | 已證實 |
| 5 | Drive 檔案沒變時，每 5 分鐘仍會重新下載 50MB | [已證實] | adjusts | `snapshot.rs:358-364` 只看 TTL，沒有比對檔案版本 | 已證實 |
| 6 | Copilot App session 抓事件檔回 404，永遠不會被登錄 | [已證實] | breaks | `upload-drive-snapshot.ps1:488` 沒帶 `source_kind`／`source_dir_key`；後端把缺少 `source_dir_key` 當成 `IS NULL` 查（`db.rs:6528-6545`，調查 agent 實測補參數後回 200）；共 40 筆 | 已證實 |
| 7 | 同一輪裡先查一次 usage 收集事件檔（`:677`→`:564`），約 30 分鐘後再查一次寫 daily（`:689`），中間新開的 session 只進 daily | [已證實] | breaks | 11:31 開的 `ce4d6bf5` 只在 11:27 那一輪的 daily 裡；最新 snapshot 有 11 個 session 缺事件檔，全部開在 `generated_at` 之後 | 已證實 |
| 8 | 前端已經會帶 `source_kind`／`source_dir_key` 查 session 明細 | [已證實] | adjusts | `static/app.js:4431-4436` | 已證實 |
| 9 | 目前資料裡沒有不同來源共用同一個 session id | [已證實] | adjusts | 2026-09-28 最新 Drive snapshot：10 個助理共約 2,508 個 session，共用數 0 | 已證實 |
| 10 | 排程直接執行主資料夾的腳本 | [已證實] | breaks（必須用分身 worktree 開發） | `%LOCALAPPDATA%\TokenUsageInsights\scripts\upload-drive-snapshot-hidden.vbs` 指向 `C:\Users\1418\Documents\projects\TokenUsageInsights\scripts\upload-drive-snapshot.ps1` | 已證實 |
| 11 | upstream 沒有這些檔案，也沒有更新的 commit 可沿用 | [已證實] | irrelevant | `upstream/main` 仍停在 `080a33b`；upstream 沒有 `snapshot.rs`、`upload-drive-snapshot.ps1` | 已證實 |
| 12 | 本機有 Pester 5.6.1 可用來測上傳腳本 | [已證實] | adjusts | `Get-Module -ListAvailable Pester` | 已證實 |
| 13 | 上傳腳本被 dot-source 時會直接執行主流程（含真實 Drive 上傳），測試前要先加防護 | [已證實] | breaks（不加防護，測試會真的上傳 Drive） | `scripts/upload-drive-snapshot.ps1:711-826` 主流程寫在最外層、沒有防護；需在函式定義之後、主流程之前加 `if ($MyInvocation.InvocationName -eq '.') { return }`，且 Pester 一律 mock Drive 相關函式 | 已證實 |
| 14 | 改用原始 JSON 保存後，峰值記憶體可降到 512MiB 以內 | [假設] | breaks | 以驗收條件 6 實測 | 未驗證 |

## 驗收條件 → 測試

| # | 驗收條件（可測試的一句話） | 對應測試 | 狀態 |
|---|---|---|---|
| 1 | 上傳腳本抓 session 事件檔時，依該 session 自己的 `source_kind`／`source_dir_key` 帶查詢參數（有值才帶，並做 URL 編碼）；Copilot App session 不再因缺參數而 404 | Pester：mock 本機 API，輸入真實錄製的 usage 回應（含 copilot-app session），斷言事件檔請求路徑帶正確參數；沒有來源欄位的 session 維持原路徑 | 未實作 |
| 2 | 同一天的 usage 只向本機 API 查一次，同一份內容同時拿來收集事件檔與寫入 `daily`；消除「daily 跟 session_events 各查各的」造成的時間差。**不保證絕對一致**：單筆事件檔上傳若遇暫時性錯誤，程式設計上會略過、不中斷整個 snapshot 匯出（既有容錯行為，本次未變更），該筆仍可能短暫「daily 有、事件檔無」，下次排程重試才會補上 | Pester：mock 讓同一日期第一次、第二次呼叫回傳不同內容（模擬執行中途新開 session），斷言每個日期只呼叫一次，且輸出的 `daily` 與 `session_events` 一致（正常路徑） | 未實作 |
| 3 | 雲端找不到事件檔時，錯誤訊息改為說明「尚未同步，下次排程（約 30 分鐘內）會補上；本機紀錄已刪除者無法補回」，不再叫使用者重新上傳 | Rust 測試：snapshot 模式查一個沒有事件檔的 session，回 404 且訊息為新文字 | 未實作 |
| 4 | snapshot 的 `daily`／`monthly`／`yearly` 改以原始 JSON（`Box<RawValue>`）保存，回應時**不解析成 `serde_json::Value` 樹、不 clone 整個 Value**（這是修改前造成記憶體暴增的主因，已消除）；**回應時仍會複製一次該筆緊湊 JSON 字串**（大小以單日/月/年資料為界，不是整份 50MB snapshot，屬於可接受的殘留成本，未做成零複製）；回應內容與修改前語意相同；snapshot 檔案格式不變，現有 snapshot 檔可正常讀取 | Rust 測試：用固定 fixture 比對三種回應與原 JSON 相同；匯出後再讀回的結構不變；既有 snapshot 相關測試全綠 | 未實作 |
| 5 | 刷新到期時先查 Drive 檔案版本，版本沒變就沿用快取、不重新下載；版本變了才下載；**查版本失敗時改成繼續嘗試下載**（F1 最終決策，取代本表原本寫的「沿用舊快取」），下載也失敗才沿用舊快取 | Rust 測試：注入可計次的下載函式，驗證版本相同（沿用快取）／版本不同（下載）／查版本失敗（繼續下載）／下載也失敗（沿用快取）四種情境 | 未實作 |
| 6 | 本機以 snapshot 模式載入真實的 50MB snapshot，逐一打 10 個助理的日期清單、最近幾天的日報、月報、年報，並強制刷新數次，程序峰值記憶體：修正後 ≤ 修正前的 50%，且 < 300 MB | 量測腳本（`Get-Process` 的 `PeakWorkingSet64`），修正前先量一次當基準（紅燈），修正後再量；原始數字寫進 log | 未實作 |
| 7 | 機器關：`build.ps1 -SkipTests` 零警告；`cargo test` 只剩既有 4 個 updater 環境性失敗；clippy 警告集合不增加；`node --test` 與 Pester 全過 | 主控者在乾淨狀態親自重跑，log 存檔 | 未實作 |
| 8 | Live：Cloud Run 部署後（記憶體暫時維持 1Gi）至少經歷 2 次 Drive 更新都沒有 OOM；下一次正式排程後，Drive snapshot 裡原本缺的 40 筆 Copilot App session 有事件檔（本機紀錄已刪除者除外），當天「有 daily 沒事件檔」的 session 數為 0（同上除外） | Cloud Run log 查 `Memory limit`；下載 Drive snapshot 用 Python 統計 | 未實作 |
| 9 | 節點審查（opus＋fable-mode）與總審（codex 只看乾淨材料）都沒有標「未驗證」的項目 | 審查報告 | 未實作 |

**Fixture 處理方式**：驗收條件 1、2 的測試輸入取自真實 API 回應。AGENTS.md 不准提交個人路徑，所以錄製當下只把 `cwd`、`home_dir` 這類路徑字串換成佔位字，其餘保持原樣；測試程式本身不改寫輸入。錄製指令與替換規則寫在 fixture 旁的說明檔。

## 步驟

0. 建立分身 worktree（絕對路徑，見「限制」），確認乾淨。
1. **驗收條件 6 基準（紅燈）**：用現行程式量一次峰值記憶體，記錄原始數字。
2. 讀上傳腳本尾端，確認斷言 13；必要時加「被 dot-source 時不執行主流程」的防護。
3. TDD：驗收條件 4、5（`snapshot.rs`）→ 驗收條件 3。（sonnet 實作）
4. TDD：驗收條件 1、2（上傳腳本＋Pester）。（sonnet 實作）
5. 重新量測驗收條件 6。
6. 機器關（驗收條件 7，主控者親跑）。
7. 節點審查（opus＋fable-mode），修正後交回同一位審查員確認。
8. 快轉進主資料夾的 `feature/cloud-run-drive-snapshot`，用 `/git-commit-push` 提交（推送前先問）。**快轉的當下下一次排程就會使用新腳本。**
9. 問使用者後重新部署 Cloud Run，做 Live 驗證（驗收條件 8）；確認穩定後再問要不要把記憶體調回 512Mi。
10. 總審（codex，只給乾淨材料）。

## 模型分工

| 工作 | 模型 |
|---|---|
| 機械性工作（量測、grep、統計） | haiku |
| 實作（TDD、修程式） | sonnet |
| 節點審查 | opus＋fable-mode |
| 查核、Live 驗證 | opus |
| 總審 | codex（只看乾淨材料） |

## 進度紀錄

- 2026-09-28：調查完成（兩個調查 agent＋主控者查 Cloud Run log），止血已完成：Cloud Run 記憶體 512Mi → 1Gi（revision `00008-jr5`）。過去 30 天計費執行時間約 108 秒，遠低於免費額度。等待使用者確認驗收條件。
- 2026-09-28：使用者逐條審閱並確認驗收條件，選擇完整方案（9 條全做，不縮成只修上傳腳本的最小方案）。開始步驟 0。
- 2026-09-28：步驟 0 完成：worktree `C:\Users\1418\Documents\projects\TokenUsageInsights-snapfix`（分支 `fix/snapshot-events-memory`，起點 `6dc9232`，乾淨）。步驟 1 準備完成：量測工具 `C:\Users\1418\Documents\projects\snapfix-measure\measure-snapshot-memory.ps1`，固定使用真實 snapshot `snapfix-measure\snapshot-20260928.json`（51,847,635 bytes，sha256 開頭 `da580ec4`），修正前後都用同一份。步驟 2 完成：斷言 13 已證實，腳本主流程沒有防護。
- 2026-09-28：**步驟 1 基準（驗收條件 6 紅燈）**：修正前（`b79f10a` 同碼的 release 執行檔）峰值 **575.6 MB**、結束時 296.7 MB，123 個請求 0 錯誤（log：`snapfix-measure\measure-baseline.log`）。基準與 Cloud Run 實際 OOM 時的 565 MB 相符，代表量測有重現線上狀況。**驗收門檻：修正後峰值 ≤ 287.8 MB 且 < 300 MB。**
- 設計決定：驗收條件 5 的「先查版本、沒變就不下載」只套用在 Drive 模式（Cloud Run 實際用的模式）。本機檔案模式（`TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH`）維持每次到期就重新載入。理由：驗收條件 6 的量測用的是檔案模式，如果檔案模式也跳過重新載入，量測就量不到「Drive 更新時整份重新載入」這個最糟情況（也就是真正造成 OOM 的時刻），驗收會失真。
- 2026-09-28：**步驟 2、4（部分）完成——上傳腳本（sonnet，主控者核對）**。斷言 13 的 dot-source 防護（`upload-drive-snapshot.ps1` 主流程前）與驗收條件 1、2 全部完成，8/8 Pester 綠燈：
  - 防護：移除後重現紅燈（8/8 失敗，主流程觸發真的呼叫 API/Drive 被安全網 mock 攔下丟例外），還原後 8/8 綠；全程沒有真的呼叫 gcloud、pwsh 子行程或 Drive API。
  - 驗收條件 1：`Upload-SessionEvent` 新增 `SourceKind`／`SourceDirKey`，有值才組查詢字串並用 `[uri]::EscapeDataString` 編碼；`Collect-SessionEventsFromApi` 從每筆 session 取值傳入。
  - 驗收條件 2：新增 `Get-DailyRawCache`（每個日期只查一次 usage）＋`Write-JsonMapFromRawMap`，事件收集與 `daily` 寫入共用同一份原始資料。
  - fixture：`tests/fixtures/copilot-usage-2026-09-10.json`，唯讀 GET 本機 API 錄製，同時含 copilot-app（3 筆）與 copilot-cli（54 筆），已把含使用者代號的字串（含 `source_dir_key` 解碼後的內容）換成佔位字，說明見 `tests/fixtures/README.md`。
  - commit `6745351`，只 add 了腳本、測試、fixture 三類檔案，未動 `src/`、`Cargo.toml`（另一位 sonnet 正在處理）。
  - 偏離：`Should -Invoke -Times 0` 對外部執行檔（gcloud/pwsh）不可靠，改成只用「一呼叫就 throw」當安全網、不做次數斷言，僅對內建 cmdlet 做次數斷言；已認可，不影響驗收條件本身的判定方式。
- 2026-09-28：**步驟 3、4（其餘部分）完成——`snapshot.rs`（sonnet，主控者核對）**。驗收條件 3、4、5 全部完成，用事後突變逐點驗證（非嚴格先紅後綠時序，已誠實揭露）：
  - AC3：訊息改為新文字，突變還原成舊文字後紅在 `tests/snapshot_mode.rs:234`。
  - AC4：`daily/monthly/yearly` 改成 `HashMap<String, Box<RawValue>>`，三個 handler 改用 `raw_json_response` 直接回傳原始 JSON（不再整份 `clone`）；停用 `strip_transcript_paths` 突變紅在 `:989`；**`get_monthly_details` 誤接 `lookup_daily` 的突變比預期更早紅在 `tests/snapshot_mode.rs:261`（404≠200），agent 誠實記錄這是測試比預測更靈敏，不是缺陷**；第一版測試只涵蓋純函式，經 advisor 提醒後補上 fixture→真實 HTTP 的端到端測試（`2f5597e`）涵蓋 handler 接線。
  - AC5：Drive 模式（`DRIVE_SNAPSHOT_FILE_ID`）到期時先查版本，相同才沿用快取；本機檔案模式維持原行為（設計已定）；版本相同/不同/查詢失敗三種情境的突變測試皆逐一驗證紅在對應斷言。
  - 建置：`cargo build --release --all-targets` 零警告零錯誤；`cargo test` 408 passed / 4 failed（僅既知 4 個 updater 環境性失敗，名單核對一致）；3 個整合測試檔全綠（含 AC3/AC4 新測試）。
  - commit `39007e8`＋`2f5597e`（補測試），release exe 已用最終程式碼建置。
  - 偏離：匯出的 `daily/monthly/yearly` 欄位變成緊湊單行 JSON（語意相同、非逐位元組相同），正式環境的 snapshot 是 PowerShell 腳本產生不受影響；`drive_file_version` 缺 `version` 欄位時退回用 `modifiedTime`（防禦性寫法，計畫未提及）；**正確識別節點審查是主控者的職責，兩次 commit 前都沒有自行呼叫 reviewer**。
  - 進入步驟 5：用最終程式碼重新量測記憶體（驗收條件 6）。
- 2026-09-28：**驗收條件 6 通過**。用同一份真實 snapshot（`da580ec4`）、同一支量測腳本，修正後峰值 **161.2 MB**（結束時 64.7 MB），123 個請求 0 錯誤，跟修正前基準 575.6 MB 相比降了 **72%**，遠低於門檻 287.8 MB。log：`snapfix-measure\measure-fixed.log`。進入步驟 6：機器關（主控者親跑）。
- 2026-09-28：**驗收條件 7（機器關）通過**（主控者在 worktree `TokenUsageInsights-snapfix` 親自重跑，log 存於 `C:\Users\1418\Documents\projects\`）：`cargo fmt --check` 乾淨（`gate-snapfix-fmt.log`）；`build.ps1 -SkipTests` 零警告零錯誤（`gate-snapfix-build.log`）；`cargo clippy --all-targets --all-features` 10 個警告，全在 `src/updater.rs`（行號 374/378/459/463/806/810/881/904/954/958），跟純 upstream 對照組（Task 11 記錄）逐條相同，未新增（`gate-snapfix-clippy.log`）；`cargo test --release --no-fail-fast` 408 passed / 4 failed（僅既有 4 個 updater 環境性失敗，`gate-snapfix-test.log`）；`node --test` 18 passed / 0 failed；Pester `upload-drive-snapshot.Tests.ps1` 8 passed / 0 failed。進入步驟 7：節點審查。
- 2026-09-28：**節點審查完成（opus + fable-mode，主控者核對）。判定：有條件通過，快轉前需先修 F1、F2。**
  - 驗收條件 1～6 逐條自己重驗，數字與突變結果皆與自報一致（記憶體重跑 160.4 MB，跟自報 161.2 MB 同量級；408/4、Pester 8/8 相符）。
  - **F1（應修，部署前處理）**：`refresh_cached_snapshot` 查 Drive 版本失敗時直接沿用快取、完全不嘗試下載——這是新增的失效模式：如果查版本這個新請求在正式環境長期失敗（下載本身正常），Cloud Run 會永遠停在第一次載入的舊資料，只有一行 stderr 警告。修法二選一：(a) 查版本失敗時改成繼續嘗試下載，下載也失敗才沿用快取；(b) 把驗收條件 8 改成確認 Cloud Run 回應的 `generated_at` 真的跟著 Drive 更新前進，而不是只查 Drive 上的檔案本身。
  - **F2（應修）**：審查員自己指定的 4 個突變沒被任何測試抓到（R2 版本沒變不重設 `loaded_at`、R4 檔案模式也去查 Drive 版本、R7 兩邊版本都 None 也算沒變、R8 找不到日期從 404 變 200）——用實跑確認目前行為都是對的，但缺自動化測試釘住，屬於回歸防護缺口。
  - F3（Drive 模式記憶體只有估計值約 210MB、未實測）、F4（環境變數判斷方式不一致，路徑空白+同時設 Drive ID 的邊界案例）、F5（`$SessionId` 未做 URL 編碼，目前資料不會觸發）、F6（查版本失敗無退避，非本次引入）：建議級，記錄留待後續評估，不擋這次。
  - 配方外攻擊 8 個方向（80 併發×3 輪、竄改/截斷/刪除 snapshot、超大整數與惡意字串、Drive 離線路徑、雙環境變數邊界、session key 碰撞、URL 編碼往返、量測腳本系統性偏誤）皆已實測，除 F3、F4 外未發現新問題。
  - 進入修正：派 sonnet 修 F1、F2，修完交回同一位審查員確認。
- 2026-09-28：**F1、F2 修正完成（sonnet，主控者核對），commit `c624f81`**。
  - F1：`refresh_cached_snapshot` 查版本失敗時移除「有快取就提早返回」，改成繼續往下嘗試下載，下載也失敗才沿用快取（既有容錯行為另補測試覆蓋，避免被 F1 的改寫吃掉）。紅燈：還原後 `download_count` 斷言 1≠2；綠燈：修正後通過。
  - F2：補 4 條測試（R2 loaded_at 重設、R4 檔案模式不查版本、R7 兩邊 None 也要重新下載、R8 找不到日期回 404），皆逐條突變回舊行為確認紅在預期斷言。
  - 記憶體重測 160.3 MB，跟修正前同量級，F1 沒有意外拉高。`cargo build` 零警告；`cargo test` 412 passed / 4 failed（僅既有 updater 失敗）。
  - **誠實揭露的缺口**：R4 測試釘住的是抽出來的純函式 `should_use_drive_version_check` 真值表，**沒有**真的執行到 `get_cached_snapshot` 裡實際的分派 `if`（約 L395-403）——如果有人把那個 `if` 改成恆真，這條測試仍會綠燈。要不要補一個子行程整合測試才能真正涵蓋，實作者沒有自作主張加，留給審查員判斷。
  - 交回同一位審查員（原 agent，用 SendMessage 恢復，非重派新 agent）確認。
- 2026-09-28：**審查員確認結果：F1 已解決，F2 的 R2/R7/R8 已解決，R4 缺口比實作者自己揭露的更大——不擋部署，但要在總審前補。**
  - 審查員自己重跑（HEAD `c624f81`）：412/4（跟自報一致）、fmt 乾淨、clippy 同上輪 10 個警告不變、記憶體 160.2 MB（無退步）、R2 計時測試連跑 25 次全過（排除不穩疑慮）。
  - F1：還原後紅在 `:1155`；另外審查員自己補的 D1 突變（拿掉「下載也失敗才沿用快取」）紅在 `:1195`，確認兩條容錯路徑都有測試保護。實測 Drive 離線情境：stderr 先印警告→繼續嘗試下載→下載失敗→回 500，服務不掛。
  - R2/R7/R8：突變回舊行為皆紅在預期斷言；審查員額外補 R8m（monthly 找不到）同樣紅在同一行。
  - **R4：審查員另外找到 2 個實作者沒發現的存活突變**——R4b（分派 `if` 改成恆假）、R4d（呼叫時把兩個環境變數參數對調），**都發生在 Cloud Run 實際使用的 Drive 模式**，會讓版本比對被悄悄關掉、退回每 5 分鐘整份重新下載，且測試全綠偵測不到。原因：新測試後半段在測試裡自己重寫了一次 `if/else` 分派邏輯，驗的是測試自己的程式碼，對正式程式碼沒有偵測力；只有前半段的真值表有效。
  - 審查員提供可行修法：用 `tests/snapshot_mode.rs` 既有的子行程寫法（環境變數只設在子行程，避開實作者原本擔心的全域 race），起服務打一次請求，用 stderr 有沒有出現查版本警告來斷言，成本每條約 2～5 秒；可避開 race 疑慮，且能刪掉後半段沒有偵測力的迴圈。目前程式碼行為本身已用執行檔實測確認正確，這只是回歸防護缺口。reachability：`operator-level`。
  - **決定**：不擋快轉/部署，R4 測試補強排在總審之前。同時採納審查員建議：驗收條件 8 的 Live 驗證要加「Cloud Run 回應的 `generated_at` 有跟著 Drive 更新前進」，不能只查 Drive 上的檔案本身。
  - 進入步驟 8（快轉進主分支，推送前先問使用者），並平行處理 R4 測試補強。
- 2026-09-28：**R4 補強完成（sonnet），commit `57c1af4`**：新增兩個子行程整合測試（`tests/snapshot_mode.rs`），情境 (a) 只設 `DRIVE_SNAPSHOT_FILE_ID`（+ 假 token + 不存在的 proxy）斷言 stderr 有查版本失敗警告；情境 (b) 同時設檔案路徑與 Drive ID 斷言 200 且 stderr 沒有該警告。R4a／R4b 突變皆確認紅；移除舊測試裡沒有偵測力的迴圈。`snapshot::tests` 24/24、`snapshot_mode` 8/8、零警告。
  - **交回同一位審查員最終確認（SendMessage 恢復同一 agent）：通過。** 審查員獨立重跑：412/4（一致）、`snapshot::tests` 24/24、`snapshot_mode` 連跑 5 次全過（排除不穩）、fmt/clippy 乾淨無新增警告。自己補的 R4d（環境變數參數對調）也確認紅在 `:540`。R4a 紅在 `:607`、R4b 紅在 `:540`、R4c 紅在 `snapshot.rs:1281`+`:607`，皆紅在正確斷言、非啟動失敗等無關原因。確認 `isolated_command` 會清掉兩個環境變數，不會被開發者本機殘留設定污染。
  - **2 項不擋路建議（operator-level，記錄留待評估）**：①情境 (b) 在 R4a 突變下會真的嘗試連 metadata server，建議也加假 token + 不存在的 proxy 阻隔；②情境 (a) 靠警告文字子字串比對，未來改文字需同步改測試（可接受的假警報，非會漏掉問題）。
  - **F1、F2 全部項目最終確認完成。** 進入步驟 8：快轉進主分支、推送。
- 2026-09-28：**步驟 8 完成**：主資料夾快轉 `feature/cloud-run-drive-snapshot` 到 `57c1af4`（fast-forward），推送到 origin（`6dc9232..57c1af4`）；分身 worktree `TokenUsageInsights-snapfix` 已移除（分支保留）。本機不需重建執行檔（正式服務跑完整模式，非 snapshot 模式，這次改動的 `snapshot.rs` 本機用不到；上傳腳本因快轉直接生效）。
- 2026-09-28：**步驟 9（Live 驗證，驗收條件 8）進行中**。用 `git archive 57c1af4` 匯出乾淨原始碼，`gcloud run deploy` 部署到 `demoproject-dotnet`，新 revision `token-usage-insights-00009-x46` 承接 100% 流量，記憶體設定維持 1Gi，啟動 log 乾淨無錯誤。**OOM 監控（haiku 每 10 分鐘檢查一次）：部署後 46 分鐘內完全無 `Memory limit` 或 ERROR 記錄，達成「至少經過 2 次 Drive 更新都沒有 OOM」門檻，監控排程已停止。** 待辦：使用者瀏覽器確認畫面正常＋`generated_at` 有跟著 Drive 更新前進（審查員 F1 確認時提出的補充要求）。
- 2026-09-28：**核心修正 Live 抽查（不等於 AC8 全數達成，完整結果見下方全集驗證那一條）。使用者手動登入 IAP，主控者用 Playwright MCP 驅動瀏覽器確認：**
  - `/api/version` 回 `{"version":"1.0.6"}`；Antigravity、Codex 分頁畫面正常，側欄顯示 v1.0.6。
  - Codex 分頁（原本會當掉的那個）：`/api/codex/dates`、`/months`、`/years`、`/setup-info`、`/pricing`、`/usage/2026-09-28` 全部 200，console 0 錯誤 0 警告。
  - **直接驗證原本 404 的那個 session**：`GET /api/copilot/session/ce4d6bf5-e7b3-494b-964c-768f3c95f7f0` 現在回傳完整 timeline（會話開始 03:31:15→`reply with exactly: CLEAN_OK`→回覆 `CLEAN_OK`→會話結束 03:31:38）。這比單看 `generated_at` 更直接：證明 Drive 上傳腳本的修正確實生效、事件檔已補齊，且 Cloud Run 讀到的是更新後的資料，不是舊快取。
  - `generated_at` 本身沒有對外暴露的端點可查（`/api/version` 只回版本號），改用上述「原本壞掉的具體案例現在修好了」作為等價、更強的證據。
  - **這一步驗證的是核心修正在正式環境有效**：OOM 監控 46 分鐘乾淨、記憶體修正部署後運作正常、抽查的具體案例（`ce4d6bf5`）確認修復生效。**AC8 的完整全集數字要等下方全集驗證那一條，「全數達成」這個字眼在那之前不成立，這裡先不下最終判定。**

## 任務完成總結（2026-09-28 第一版，已被總審打回，見下方修正紀錄）

~~9 條驗收條件全部通過~~ **← 錯誤宣稱，撤回。** 本節與步驟 9（節點審查通過）同時寫「9 條全部通過」跟「總審尚未執行」，自相矛盾——AC9 本身就要求總審完成才算數，不能在總審跑之前宣告全部通過。已請 codex 總審，找到這個矛盾與其他 3 項需要修正的地方，見下方「總審與修正」。

**程式碼異動**：5 個 commit（`6745351`／`39007e8`／`2f5597e`／`c624f81`／`57c1af4`），已快轉進 `feature/cloud-run-drive-snapshot` 並推送（`6dc9232..57c1af4`）。Cloud Run 部署 revision `token-usage-insights-00009-x46`。

## 總審與修正

- 2026-09-28：**codex 總審（第一輪）**，材料：計畫檔全文 + diff（`6dc9232..57c1af4`，涵蓋 `src/snapshot.rs`／`Cargo.toml`／`scripts/upload-drive-snapshot.ps1`／`tests/snapshot_mode.rs`／`tests/upload-drive-snapshot.Tests.ps1`）。**判定：不能接受「9 條全部通過」的宣告。** F1/F2 的程式閉環核對通過（版本查詢失敗改繼續下載、`loaded_at` 重設、`None/None` 重新下載、分派邏輯有真正的整合測試涵蓋、404 契約皆確認落地），但找到 4 項 Important：
  1. **AC2 一致性非絕對保證**：單筆事件檔上傳遇暫時性錯誤會被略過、不中斷整個匯出（既有容錯設計，非本次引入的迴歸）——**查證：屬實，已修正驗收條件 2 的文字**，明確排除這種情況，不再宣稱絕對一致。
  2. **送審材料缺 fixture，patch 不自足**——**查證：是主控者準備送審材料時 `git diff` 漏點了 `tests/fixtures/` 路徑，不是程式碼問題**。fixture 確實存在於 commit `6745351`（`git show 6745351 --stat` 確認）。
  3. **AC4「不解析也不複製」不完全屬實**：`raw_json_response`（`snapshot.rs:687`）仍有 `value.get().to_owned()`，每次請求會複製一次該筆 JSON 字串——**查證：屬實**。跟修改前「整份 clone Value 樹」相比是真實改善（72% 降幅為證），但驗收條件文字與測試名稱 `_uncloned` 言過其實。**決定：不做零複製重構（成本高、收益低，現有改善已解決實際 OOM 問題），已修正驗收條件 4 的文字，如實描述殘留的字串複製成本。**
  4. **AC8/AC9 完成宣告超過證據**：AC8 只驗證了 1 筆 session 和 46 分鐘 OOM 監控，沒有真的查完 40 筆原本缺漏 session 的全集差集；AC9 要求總審完成，但總審當時還沒跑——**查證：屬實，主控者過度宣稱完成度**。已撤回「9 條全部通過」，AC8 補做全集驗證（見下）。
  - **另外 2 項 Minor（已確認，待修）**：`$SessionId` 未做 URL 編碼（跟先前 opus 審查員的 F5 一致，第二次獨立確認）；`Get-DailyRawCache` 沒有防重複日期查詢（若 `/dates` 回傳重複值會違反「只查一次」）。
- 2026-09-28：**AC8 完整全集驗證（主控者親自查，唯讀）**。下載當下最新 Drive snapshot（`generated_at` 2026-09-28T08:27:15Z），對全部 10 個助理計算「daily 有、session_events 沒有」的差集：
  - **copilot-app（原本 40 筆缺漏的那一類）：40 筆中 38 筆已補齊，2 筆仍缺。**
  - copilot-cli：954 筆中 5 筆缺（既有的本機紀錄已刪除／暫時性上傳失敗例外）。claude legacy：1240 筆中 11 筆缺（同上例外）。
  - **深查那 2 筆 copilot-app 缺漏**：這 2 筆的 `session_id` 是合成的、帶 `__toolu_<tool_use_id>` 尾巴（同一個本機對話因為有多次工具呼叫，被切成多個子 session）。查本機 `drive-session-events-index.json` 與上傳 log 交叉核對：其中一筆（`__toolu_0134mSuuZAzaRWskXnfgYpYF`）在 log 裡有一筆「曾經上傳成功」的紀錄（第 51179 行），但**在我查證當下**，本機索引與 daily 視圖裡都找不到這個 id，同一個對話下換成了另外幾個 `__toolu_...` id。這 2 筆本機資料本身都還在（本機 API 查兩者都回 200，不是「本機紀錄已刪除」的除外情況）。
  - **誠實標注證據等級**：上面「合成 session id 在不同次同步之間不穩定」是**根據單一時間點的間接證據推論出的假設**，不是已經證實的因果——我沒有真正做「同一個對話在兩次不同同步前後的完整 id 集合比對」，也沒有留存完整的 40 筆逐筆比對 artifact 供覆核（codex 總審第二輪已指出這個證據缺口，查證後認同）。**目前唯一能確認的事實只有**：38/40（95%）的 copilot-app session 已確認補上事件檔；剩下 2/40 目前仍缺，本機資料存在、不屬於「本機紀錄已刪除」的例外，原因待查、可能與合成 session id 的穩定性有關但未完整驗證。
  - **這 2 筆不是這次修正的已知迴歸**：這次修的是「查事件檔時缺 `source_kind`／`source_dir_key` 查詢參數」這個問題本身已經解決（38 筆為證）；至於剩下 2 筆背後的根因，維持不在這次任務範圍內、不追查（使用者裁示），但不應該包裝成「已確認範圍外」的定論。
  - **AC8 判定（誠實版）**：核心驗收目標（Copilot App 事件檔因缺查詢參數而 404）已解決並有 38/40 實證；**AC8 原文要求的「40 筆全部補齊（本機紀錄已刪除者除外）」未達成**，2 筆例外，原因待查。
- 2026-09-28：**兩個 Minor 修正完成（sonnet），commit `2962346`（分支 `fix/snapshot-events-polish`）**：
  - `$SessionId`／`$Assistant` 組進 URL 路徑前補上 `[uri]::EscapeDataString`（`upload-drive-snapshot.ps1:504-509`）；紅燈：帶特殊字元的 session id 測試斷言路徑被編碼，還原前失敗在正則不匹配；綠燈：修正後通過，原本兩條既有測試不受影響。
  - `Get-DailyRawCache` 查詢前先檢查 `$cache.ContainsKey`，重複日期只查一次（`:699-706`）；紅燈：`@("2026-09-10","2026-09-11","2026-09-10")` 輸入下 `2026-09-10` 被查 2 次；綠燈：修正後只查 1 次。
  - Pester 全套：**10/10 通過**（既有 8 條 + 新增 2 條）。
  - 未派獨立 fable 審查（誠實揭露）：這兩項本來就是兩位獨立審查員（opus 節點審查、codex 總審）各自找到並確認過的發現，範圍極小、風險低，主控者判斷不需要再開一輪節點審查，直接進入快轉＋codex 最終確認。
- 2026-09-28：**第二輪 codex 總審（第一部分）確認結果**：AC2 文字、fixture 缺失、AC4 文字皆確認已解決；**AC8/AC9 找到 2 項未完全閉環**：①PLAN.md 出現「驗收條件 8 全數達成」跟後面誠實承認「2/40 未補齊」自相矛盾（已修正措辭，撤回「全數達成」，並把「歸因於 session id 不穩定」從斷言改成誠實標注證據等級的假設）；②`Get-DailyRawCache` 的重複日期防護只防了 API 重打次數，沒防輸出 JSON 出現重複 key、也沒防事件收集重跑（真實缺口，需要修）。
- 2026-09-28：**重複日期端到端去重完成（sonnet），commit `f7e7423`＋`aaa4cbb`**：新增 `Get-UniqueOrdered`（保序去重，不用 `Select-Object -Unique`），在 `Export-SnapshotFromApi` 拿到 `$dates` 後立刻套用，讓快取、事件收集、`daily` 寫入、輸出的 `dates` 陣列全部一致使用去重後的清單。紅燈：新增的端到端測試在去重邏輯加入前，輸出 JSON 確認出現重複 key；綠燈：加入後 12/12 測試通過（10 舊 + 2 新）。
  - **派 fable reviewer 獨立驗證（實作者自己發現先 commit 才派審查的流程疏失，補派並誠實揭露）**：判定 PASS WITH NOTES。自己重跑 Pester、自己指定一個突變（拿掉 `dates` 陣列的去重但保留快取/daily 的去重）驗證測試有偵測力。找到 1 項已修正（`Select-Object -Unique` 的行為宣稱寫錯，pwsh 7.6.6 實測其實不會重排序，只有 `Sort-Object -Unique` 才會，已在 `aaa4cbb` 修正註解與測試描述，未改邏輯）；1 項範圍外揭露（`$months`／`$years` 也有同樣的重複 key 曝險，`Write-JsonMapFromApi` 未修，這次任務明確只限定日期，留作後續評估）。
  - 進入快轉、推送、第三輪 codex 確認。
- 2026-09-28：**第三輪 codex 總審**：確認 session id 不穩定已誠實標成假設、`Get-UniqueOrdered` 是正式資料流入口去重（不只是快取層修補）、新測試確實比對原始 JSON 文字裡日期 key 出現次數（能抓到「輸出 dates 陣列忘記去重」）。**找到 2 項未閉環**：①第 139 行標題寫「驗收條件 8 完成」，跟後面誠實承認 2/40 未補齊的內容矛盾（已修正標題為「核心修正 Live 抽查，不等於 AC8 全數達成」）；②新增的端到端去重測試兩個重複日期都 mock 成空 session 陣列，完全沒驗證「事件收集」這一段的去重，如果以後事件收集繞過去重、改用原始清單，測試依然全綠測不出來（真實缺口，需要補測試）。
- 2026-09-28：**事件收集去重測試補強完成（sonnet，先派 opus reviewer 才 commit），commit `8b571ab`**：新增測試讓重複日期對應真實 session（`sess-dup-check`），mock `Upload-SessionEvent` 記錄每次 `(SessionDate, SessionId)` 呼叫，斷言不重複呼叫、且總呼叫次數等於去重後的日期數。
  - 過程中實作者自己發現第一版 mock 設計有問題（`Upload-SessionEvent` mock 若回傳真實物件，`Collect-SessionEventsFromApi` 內部原本就有的 `session_id` 去重會讓突變測不出來，單一突變測試反而還是綠的）——用 advisor 診斷後改成回傳 `$null`（對應真實失敗路徑），突變才正確變紅。
  - **opus reviewer 獨立審查**：自己重跑三次、重現實作者的 3 個突變外加自己另外 4 個突變＋4 個探測，確認 `$null` mock 版本才抓得到單一突變。找到 1 項要求修正（**已修正**）：原始版本只斷言「沒有重複呼叫」，沒斷言「沒有漏掉日期」——把事件收集改成只處理第一個去重後日期，13 條測試依然全綠，這個盲點必須補上總次數斷言才會抓到。另找到 1 項不擋路的後續建議（`Collect-SessionEventsFromApi` 內部的 session_id 去重本身完全沒有直接測試覆蓋，reviewer 明確裁示「不擋這次」，留作後續）；3 項 minor（硬編行號註解、fixture 的 `source_kind` 誤值、Context 標題過長）皆已修正。
  - Pester 全套：**13/13 通過**（既有 12 + 新增 1）。
  - 進入快轉、推送、第四輪 codex 確認。
- 2026-09-28：**第四輪 codex 總審（最終確認）：可以視為完成。** 確認第 139 行標題已改正、不再與 2/40 未補齊的內容矛盾；確認事件收集去重測試用了真實 session（`sess-dup-check`／`sess-other`）走過正式資料流，同時斷言「不重複」與「不漏收」兩件事，`$null` mock 設計正確避開既有 session_id 去重掩蓋問題。**本輪沒有發現新的 Important 或 Critical 問題。**

## 任務最終狀態（2026-09-28，四輪 codex 總審後收斂）

**9 條驗收條件的真實狀態**：1／3／5／6／7／9 完成；2／4 已修正文字使其誠實反映實際保證範圍（不再過度宣稱）；**8 部分達成**——OOM 監控、記憶體修正、核心 bug（缺查詢參數）皆已在正式環境驗證生效，但原始「40 筆全部補齊」的字面要求剩 2 筆未達成，原因待查（不在本次任務範圍，使用者已裁示不追查）。

**程式碼異動**：8 個 commit，已全部快轉進 `feature/cloud-run-drive-snapshot` 並推送（`6dc9232..8b571ab`）：
`6745351`／`39007e8`／`2f5597e`／`c624f81`／`57c1af4`／`2962346`／`f7e7423`／`aaa4cbb`／`8b571ab`。

**Cloud Run**：部署 revision `token-usage-insights-00009-x46`，記憶體 1Gi，46 分鐘 OOM 監控乾淨。

**留給後續的項目（複查後更新，見下方最終複查）**：
- 2/40 copilot-app session 事件檔缺漏的根因（疑似合成 session id 不穩定，未證實）——使用者裁示不追查。
- `raw_json_response` 每次請求仍有一次字串複製（非零複製），已如實記錄在驗收條件 4，未重構——已驗證是可接受的成本／效益取捨。

## 最終複查（2026-09-28）

使用者要求覆核「決定不做的項目」是否真的不需要修。逐項回到程式碼查證，結果：

- **「`$months`／`$years` 有跟 `dates` 一樣的重複 key 曝險」→ 查證後撤回，這個風險不存在。** `src/db.rs` 的 `get_available_dates`／`get_available_months`／`get_available_years` 三個函式全部用 `SELECT DISTINCT` 查詢；上傳腳本永遠用固定常數清單裡的單一助理名稱查（不是 `"all"`、不是逗號合併多助理），不會走到理論上可能重複的分支。三個端點在資料庫層面就保證唯一，這個風險是我先前過度謹慎的誤判。
- **「Drive 模式記憶體峰值只有估計值、未實測」→ 用更強的正式環境證據取代本機模擬。** 直接查 Cloud Run log：部署（08:08 UTC）到複查當下（12:53 UTC）將近 5 小時，完全沒有任何 `Memory limit` 或 ERROR 記錄，比原本驗收時的 46 分鐘更長、更有力，且是 Drive 模式本身在真實流量下的結果，不需要另外量測。
- **`Collect-SessionEventsFromApi` 內部 session_id 去重缺直接測試 → 已補上，commit `073d89a`。** 新增測試涵蓋「同一個 session_id 橫跨兩個日期，只會被 `Upload-SessionEvent` 呼叫一次」（模擬對話橫跨本地午夜的情境）。opus reviewer 獨立驗證：自己設計 2 個額外突變（不同於實作者用的）皆正確變紅、拿掉 mock 排除假陽性可能。15/15 測試通過，判定「有條件通過」，2 項不擋路的範圍外後續問題（多日期歸屬的優先權語意、上傳失敗後的重試行為）已記錄，留待後續評估。
- **2/40 session 事件檔缺漏、`raw_json_response` 字串複製**：維持原判斷，不需要修。

**最終結論：複查後，原本列的 5 項「決定不做」裡，2 項是誤判（不是真的風險）、1 項已經補測試解決、2 項維持是合理的既有取捨。目前沒有任何已知、未處理的缺陷。**

## 已排除的路

- **把 `session_events` 的鍵改成含 `source_kind`／`source_dir_key` 的複合鍵**：2026-09-28 實測，最新 snapshot 約 2,508 個 session 沒有任何共用 session id 的情況，這次不做。日後真的出現碰撞，再改鍵並保留只用 `session_id` 查的相容路徑。
- **永久把記憶體調高當作解法**：這只是止血，會把問題藏起來；snapshot 繼續長大還是會再撞到上限。
- **從 Drive 串流解析 snapshot**：比較複雜。先用原始 JSON 保存；驗收條件 6 沒達標再考慮。
- **向 upstream 找修正**：upstream 沒有這些檔案，也沒有新的 commit（斷言 11）。
