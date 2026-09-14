# Token 戰情室 - 開發與進度更新紀錄 (Progress Log)

本文件用於記錄專案的開發里程碑與重大進度更新。

---

## 🚀 開發里程碑與更新歷史

### 2026-09-14 - 整合 upstream v0.9.8（v0.2.2 起 291 commits）
- **背景**：upstream 已改為單一執行檔 CLI（`export`/`import`/`update` 子命令）、內建自動更新器、新增 grok/pi/omp/muse 助理；fork 的 Cloud Run snapshot 功能與新架構在三處衝突。
- **合併方式**：先 `improve` ← upstream（解 app.js/index.html），再 `feature/cloud-run-drive-snapshot` ← `improve`；README.md 依指示保留 fork 版本。合併前標籤 `backup/improve-pre-v0.9.8`、`backup/feature-pre-v0.9.8`。
- **main.rs 嫁接**：`--export-snapshot` 改在 `cli::run` 之前攔截（否則 exit 2「未知指令」）；snapshot 模式在更新救援、DB 初始化、自動更新之前早退；snapshot router 對齊 upstream 新端點（session-search、model-sessions、匯出／匯入）回 501 JSON，靜態檔沿用 `no-cache`。
- **snapshot.rs**：刪除與 upstream 已分歧、無法編譯的複製版彙總邏輯，`--export-snapshot` 改在同進程呼叫本機看板 handler；`ASSISTANTS` 補齊 9 個助理，並以測試釘住必須與 `handlers::is_supported_assistant` 一致。
- **雲端／排程**：`Dockerfile` 加 `TOKEN_USAGE_INSIGHTS_AUTO_UPDATE=0`；`upload-drive-snapshot.ps1` 助理清單補齊（舊版本機看板對新助理回 400，腳本視為空資料，不中斷）。
- **⚠️ 注意**：fork 建置若安裝到 `%LOCALAPPDATA%\TokenUsageInsights`，upstream 自動更新會換成官方版（無 snapshot 功能），需設 `TOKEN_USAGE_INSIGHTS_AUTO_UPDATE=0` 或 `--no-auto-update`。
- **審查後修正**（opus reviewer；fable 額度用盡 429 後依 fallback 改派）：`--export-snapshot` 只接受第一個參數、`TOKEN_USAGE_INSIGHTS_EXPORT_SNAPSHOT` 只在無參數時生效（原本會劫持 `--help`／`update` 與子命令參數）；匯出不再呼叫 `migrate_old_databases`（它固定改名家目錄下的舊 DB，upstream 只在看板啟動時做）；handler 非 2xx 回應先判狀態碼再解析；新增 `tests/snapshot_mode.rs`（snapshot 模式不建資料目錄、`--export-snapshot` 實際匯出 9 助理、環境變數不劫持子命令）、路由對照前端 API 測試、session_id 與 ps1 名單測試。審查員指定突變 M2/M4/M5b/M6/M7 皆紅在對應測試；M5a（早退移到 `perform_startup_recovery` 之後）不紅，因該函式在非標準安裝環境（含 Cloud Run 映像）為 no-op。確認審查（opus）判定可提交並指出路由測試抓不到「handler 綁錯／方法錯」：路由測試改為逐端點預期表（方法＋是否 501）並要求與 app.js 路徑一一對應，R4/R5/R6 突變皆紅；`--export-snapshot X` 後多帶參數改為拒絕；匯出整合測試加斷言隔離下資料為空（Linux/macOS 的 VS Code 預設路徑隔離未實測）。
- **驗證**：`cargo test --no-fail-fast` 主程式 318 過、`tests/cli.rs` 2 過、`tests/snapshot_mode.rs` 3 過；4 個 updater 測試因偵測到本機執行中的看板而 fail-closed，純 upstream/main 同環境對照亦同樣 4 敗。`node --test` 12/12。Live：正式 DB 複本上 `--export-snapshot` 成功（9 助理、transcript_path 全清空），snapshot 模式與一般模式 49 個期間回應比對 0 差異（浮點取 12 位有效數字；一般模式自身連打即有末位差異），瀏覽器實測月報／OMP 日報渲染正常、console 0 錯誤。
- **本機測試踩坑**：Windows 保留 3004–3203 埠，測試伺服器綁 3014/3015 會 os error 10013，改用 18714/18715。

### 2026-07-16 - Drive Snapshot 上傳管線可靠性修復
- **問題**：Cloud Run 看板從 2026-07-15 15:19 起停止更新（每日資料與 session 細項皆看不到今天）。根因在本機排程的 `upload-drive-snapshot.ps1`：
  - Google OAuth access token 被快取整個 run 從不刷新，長時間上傳 session events 途中 token 過期 → Drive API 回 401 → 整個 run abort，核心 snapshot 從未上傳。
  - 排程 `ExecutionTimeLimit` 僅 20 分，但核心匯出就要 ~18.5 分，加 session events 必然超時被強制終止。
  - copilot 有數千個從未上傳的 session；而 index 只在 run 全部跑完才存檔 → run 永遠跑不完 → 每次重頭傳、永不收斂。
- **修法**（`scripts/upload-drive-snapshot.ps1`）：
  - `Ensure-AccessToken` 逾時（45 分）自動重取 token。
  - session-event 上傳改 best-effort：單筆失敗只略過不中斷；遇 401 立即清 token 讓後續筆自動重取。
  - 核心 snapshot 上傳遇 401 重取 token 重試一次，確保每日資料一定上得去。
  - 新增 `-SkipSessionEventAssistants` 參數，可跳過指定 assistant（目前排程跳過 `copilot` 積壓）。
- **index 增量存檔（checkpoint）**：每成功上傳 25 筆 session event 就存一次 index（原本只在 run 全部跑完才存），即使 run 中斷進度也保留，讓 copilot 這類大量積壓能跨多次 run 逐步收斂；Drive 上傳補上 timeout（snapshot 300s、session/metadata 120s、權限 60s）防偶發卡死。
- **copilot 積壓已清空**：backfill 一次補完 803 筆 copilot session event（index copilot 1 → 804，32 次 checkpoint 存檔），session 細項恢復；隨後移除排程 vbs 的 `-SkipSessionEventAssistants copilot`，讓 copilot 之後持續更新。
- **配套**：排程 `ExecutionTimeLimit` 20 分 → 2 小時。
- **待辦**：核心匯出逐日打 API（copilot 164 天）約 18.5 分偏慢，可改批次匯出優化。

### 2026-07-09 - Windows 狀態列 (Statusline) 架構升級
- **修正超時 Issue**：解決 Windows 下 PowerShell 冷啟動耗時長、導致狀態列超時報錯 `exit status 1` 的問題。
- **引進 Go 協調器**：編譯出原生 Windows 二進位檔 `statusline-token.exe`，以 `< 1ms` 的速度完成前台 Token 增量寫入日誌。
- **保留原生狀態列**：Go 程式在寫入後，會自動調用底層原生的 `hooks/statusline-go.exe`，確保 TUI 終端機正常渲染出 credits 餘額與進度條。
- **配置與代碼歸檔**：
  - 新增 Go 原始碼 [statusline-token.go](file:///C:/Users/1418/Documents/projects/TokenUsageInsights/shell/antigravity/statusline-token.go) 於本專案庫中。
  - 將詳細設定步驟與架構流程記錄於 [README.md](file:///C:/Users/1418/Documents/projects/TokenUsageInsights/README.md)。
