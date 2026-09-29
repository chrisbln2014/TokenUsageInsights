# Fixtures

## copilot-usage-2026-09-10.json（`upload-drive-snapshot.Tests.ps1`）

- **來源**：本機正式看板 API，唯讀 GET：
  ```
  curl -s http://localhost:3003/api/copilot/usage/2026-09-10
  ```
- **錄製日期**：2026-09-28。
- **挑選理由**：先用 `GET /api/copilot/dates` 列出日期，逐日用上面的指令查
  `sessions[].source_kind`，找到 2026-09-10 同時含有 `copilot-app`（3 筆，
  皆帶非空 `source_dir_key`）與其他來源（`copilot-cli`，54 筆）的 session，
  且檔案大小適中（約 130KB）。
- **替換的欄位**（只替換路徑相關字串，其餘內容保持原樣）：
  - 字串 `1418`（使用者帳號目錄名）→ `REDACTEDUSER`。這個取代同時處理了
    `home_dir`、`cwd`、`session_name` 內文字提到的路徑等所有明文出現的位置。
  - `sessions[].source_dir_key` / `raw_entries[].source_dir_key`：這個欄位是
    「路徑的 hex 編碼」（`db.rs` 的 `source_dir_key` 註解：hex-encoded
    canonical path），字串替換抓不到，因為編碼後的十六進位文字不會直接出現
    `1418` 這四個字元。解碼後原始值為 `\\?\C:\Users\1418\.copilot`；改成先
    解碼確認內容，再對明文路徑做同樣的 `1418` → `REDACTEDUSER` 取代，重新
    hex 編碼回去，取代掉檔案裡所有出現這個 hex 值的地方（`sessions` 與
    `raw_entries` 共 13 處）。
  - `transcript_path`：此日期所有 session 的 `transcript_path` 本來就是空字
    串，未做任何處理。
- **測試程式本身不改寫這份 fixture**：Pester 測試把整份檔案原文當成
  `Invoke-TokenUsageApiRaw`／`Invoke-TokenUsageApi` 的 mock 回傳值使用；驗收
  條件 2「第二次呼叫多一個 session」的情境是在測試程式裡另外用
  `ConvertTo-Json` 從記憶體物件組出的第二份內容（模擬執行中途新開
  session），不是修改這份 fixture 檔案。

## all-scope-claude-codex-snapshot-slice.json（`tests/snapshot_mode.rs`）

- **來源**：本機正式 Cloud Run snapshot 匯出檔
  `C:\Users\<使用者>\AppData\Local\TokenUsageInsights\snapshot.json`（`--export-snapshot`
  的輸出／`upload-drive-snapshot.ps1` 上傳前的本機檔案），**匯出時的二進位版本早於這次
  「全部 Agent」合併改動**，因此 `daily_breakdown`／`monthly_breakdown` 每個項目天生就沒有
  巢狀的 `agents` 欄位（只有 `MonthlyDetailsResponse`／`YearlyDetailsResponse` 頂層的
  `agent_breakdown` 存在）——這正好拿來當「舊格式相容性」的真實 fixture，不用另外偽造缺欄位
  的假資料。
- **錄製日期**：2026-09-29（讀取既有本機檔案，非另外重跑指令產生）。
- **挑選理由**：用 `jq`/`python3` 檢視 `.assistants` 底下每個 assistant 的
  `dates`/`daily`/`monthly`/`yearly`，找 `claude`／`codex` 兩者都有資料、且日期
  `2026-08-06`、月份 `2026-08`、年份 `2026` 彼此對應（`monthly.daily_breakdown` 含
  `2026-08-06`、`yearly.monthly_breakdown` 含 `2026-08`）、檔案大小適中的切片（原始
  `daily["2026-08-06"]` 兩邊合計約 10.5KB；`copilot` 同一天的 `daily` 因為有 532 個
  session、`yearly` 因為有 938 筆 `projects` 太大，所以這份 fixture 只保留
  `claude`／`codex` 兩個 assistant）。
- **切法**：只保留 `schema_version`／`generated_at`／`source` 三個頂層欄位＋
  `assistants.claude`／`assistants.codex` 兩個 assistant；每個 assistant 底下
  `dates`／`months`／`years` 保留完整真實陣列（沒有裁切內容，只是全部帶著），
  `daily`／`monthly`／`yearly` 只留下 `2026-08-06`／`2026-08`／`2026` 這三個 key
  對應的完整真實內容（切下來的當下逐位元組保留，`cwd`／`session_name` 後續的代號
  改寫見下方「替換的欄位」小節，其餘欄位未動過）；
  `session_events` 清空成 `{}`（這個欄位跟本次要測的「全部 Agent」合併邏輯無關，
  claude 有 1050 筆、codex 有 326 筆，留著只會讓檔案暴增到 300KB+ 卻測不到任何東西）。
- **替換的欄位**：
  - 字串 `1418`（使用者帳號目錄名）→ `REDACTEDUSER`，共 377 處，涵蓋 `home_dir` 等
    明文出現這個帳號目錄名稱的位置。這份切片沒有 `source_dir_key`（該欄位只在
    Copilot App 來源出現，`claude`／`codex` 一律是 `null`），所以不需要像
    `copilot-usage-2026-09-10.json` 那樣額外處理 hex 編碼。
  - **`cwd`／`session_name` 一致性代號改寫（2026-09-29 第二輪，補做隱私修正）**：
    第一輪只換掉帳號目錄名，但 `cwd`（主要來自 `monthly`／`yearly` 的 `projects`
    清單）裡還留著 321 個不重複的真實專案路徑，`session_name`（`sessions`／
    `raw_entries`）裡還留著 2 則使用者原始提問全文（含真實公司名稱與客戶／專案代
    號）——這個 repo 的 `origin` 是公開 repo，一旦 commit 會無法收回，因此第二輪
    用一次性腳本（跑完即丟，沒有留在 repo 裡）把這兩個欄位改寫成代號：
    - **改寫規則**：遍歷順序固定為 `claude`→`codex`、`daily`（依日期字串排序）→
      `monthly`（依月份排序）→`yearly`（依年份排序），每個欄位第一次出現時依序
      分配代號；同一個原始值（`cwd` 以**小寫正規化**比對，因為 Windows 路徑不分
      大小寫，且已實測抓到 2 組僅磁碟機代號大小寫不同的路徑）永遠對應同一個代
      號，不同原始值一定得到不同代號。
    - **`cwd`** → `/project-001`、`/project-002`……依出現順序編號，目前檔案內共
      319 個不重複代號（321 個原始字串小寫去重後）。**這個對應關係跨 `claude`／
      `codex` 保留**：改寫後仍可驗證 `claude.monthly["2026-08"].projects` 與
      `codex.monthly["2026-08"].projects` 有 3 個共用的 `cwd` 代號（`yearly` 有
      8 個），確保「兩個 assistant 剛好用同一個 cwd 時 `merge_projects` 會正確合
      併成一筆」這個測試情境沒有被破壞。
    - **`session_name`**：這份切片裡的 2 個 `session_name` 值都是完整的自然語言
      提問全文（不是單純的路徑衍生 ID），一律改寫成 `session-prompt-001`／
      `session-prompt-002`（依出現順序編號，同一問句在 `sessions` 與
      `raw_entries` 裡出現多次都對應同一個代號）。
    - **驗證**（改寫後重新掃過整份檔案，不是沿用改寫前的結果）：`grep -c 1418`
      回 0；`grep -o -E "[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"` 掃全檔
      0 命中；改寫前出現過的真實識別資訊（公司名稱、客戶／專案代號、原始問句全
      文）逐一 `grep` 確認 0 命中；檔案裡已經沒有任何非 ASCII 字元（改寫前的中文
      提問內容曾經是檔案裡僅有的非 ASCII 來源）。
    - **改寫用的「原始值 → 代號」映射表不保留**：只在腳本執行期間的記憶體裡維
      護，寫完檔案就捨棄，不落地成任何檔案——保留它等於換一種方式把真實路徑存
      進 repo。
- **測試程式本身不改寫這份 fixture**：`tests/snapshot_mode.rs` 用
  `std::fs::copy` 把整份檔案原封不動複製成子行程的 `snapshot.json`，讀取
  子行程回應後才在記憶體中組「預期值」（例如把 `/api/claude/...` 與
  `/api/codex/...` 兩個單一 assistant 回應的欄位相加，去跟 `/api/all/...`
  的回應比對），不會在餵給受測程式之前修改 fixture 檔案本身。
