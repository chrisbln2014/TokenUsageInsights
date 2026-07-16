# Token 戰情室 - 開發與進度更新紀錄 (Progress Log)

本文件用於記錄專案的開發里程碑與重大進度更新。

---

## 🚀 開發里程碑與更新歷史

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
