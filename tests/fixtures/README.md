# Fixtures：upload-drive-snapshot.Tests.ps1

## copilot-usage-2026-09-10.json

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
