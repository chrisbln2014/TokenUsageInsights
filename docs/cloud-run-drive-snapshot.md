# Cloud Run 讀 Google Drive Snapshot

這個模式讓 Cloud Run 只提供唯讀看板，資料不放 GCS、不連本機 SQLite。資料來源是本機定期匯出的 `snapshot.json`，再透過 Google Drive 分享給 Cloud Run 的 service account 讀取。

## 架構

```mermaid
flowchart LR
  A["Local TokenUsageInsights"] --> B["Export snapshot.json"]
  B --> C["Google Drive"]
  D["Browser"] --> E["Cloud Run + IAP"]
  E --> F["Drive API"]
  F --> C
```

## Snapshot 內容

Snapshot 預先產生前端看板需要的每日、每月、每年 API response：

- `dates`, `months`, `years`
- `usage/:date`
- `monthly/:year_month`
- `yearly/:year`

Cloud Run snapshot 模式不包含本機 transcript timeline。`raw_entries.transcript_path` 也會被清空，避免把本機日誌路徑放到雲端。

涵蓋的助理：antigravity、copilot、codex、claude、cursor、grok、pi、omp、muse、mcode（`src/snapshot.rs` 的 `ASSISTANTS`，由測試釘住必須與 `handlers::is_supported_assistant`、`scripts/upload-drive-snapshot.ps1` 的 `$Assistants` 一致）。`--export-snapshot` 直接呼叫本機看板的 handler 產生 response，內容與本機 API 完全一致。

`--export-snapshot <path>` 必須是第一個參數（不可與 `export`／`import`／`update` 等子命令混用）；`TOKEN_USAGE_INSIGHTS_EXPORT_SNAPSHOT` 環境變數只在不帶任何參數執行時生效。匯出會先增量同步本機日誌，但不做舊版資料庫遷移（由看板啟動時處理）。每日回應包含 `home_dir` 與各 session 的 `cwd`、`session_name`（前端用來分組與縮寫路徑），只有 `transcript_path` 會被清空。

Snapshot 模式唯讀，以下功能回 `501` JSON 錯誤：Session 提示詞搜尋、模型 Session 明細、單日匯出／匯入、匯入批次查詢與回滾。

## 自動更新：本 fork 已永久停用

upstream v0.9.8 起看板預設會自動更新，下載來源固定是 `doggy8088/TokenUsageInsights` 的官方 Release（不含本 fork 的 snapshot 功能）。本 fork 雲端與地端都**一律不自動更新**，upstream 的新版本改由維護者手動合併程式碼。

- 地端：`src/updater.rs` 的 `FORK_AUTO_UPDATE_DISABLED` 讓背景自動更新永遠停用；`TOKEN_USAGE_INSIGHTS_AUTO_UPDATE=1`、`config.yaml` 的 `auto_update: true` 都無效（`tests/auto_update_disabled.rs` 以模擬標準安裝驗證）。
- Cloud Run：snapshot 模式在啟動時就跳過更新流程，`Dockerfile` 也設 `TOKEN_USAGE_INSIGHTS_AUTO_UPDATE=0`。
- ⚠️ 手動 `token-usage-insights update` 子命令仍在，執行後會下載官方版覆蓋本 fork，不要使用。

## 本機匯出

```powershell
pwsh -ExecutionPolicy Bypass -File .\scripts\export-snapshot.ps1 `
  -OutputPath "G:\My Drive\TokenUsageInsights\snapshot.json" `
  -ExePath ".\target\release\token-usage-insights.exe"
```

也可以直接用執行檔：

```powershell
.\token-usage-insights.exe --export-snapshot "G:\My Drive\TokenUsageInsights\snapshot.json"
```

建議用 Windows Task Scheduler 每 15 到 60 分鐘跑一次。頻率越高，Drive API 與 Cloud Run 讀取壓力越高；個人看板通常 30 分鐘已足夠。

## 實際部署位置（本機這套環境）

| 用途 | GCP project | 說明 |
|---|---|---|
| **Cloud Run 服務** | `demoproject-dotnet`（asia-east1，服務名 `token-usage-insights`） | 有綁帳單帳戶，用量超出免費額度會直接計費 |
| Drive API 配額（本機上傳腳本的 ADC） | `tokenusage-chris-20260709` | 只當 `X-Goog-User-Project` 配額 project 使用；**沒有綁帳單，不能拿來跑 Cloud Run** |

兩者用途不同，不要混淆：本機上傳腳本的 `-ProjectId` 是 Drive API 配額 project，不是 Cloud Run 所在的 project。

## 自動上傳到 Google Drive

如果不使用 Google Drive 桌面同步程式，可以改用 `gcloud` 的 Application Default Credentials 取得 `chris@berlin.com.tw` 的 OAuth token，再由腳本直接呼叫 Drive API。

首次使用先登入 ADC，scope 使用 `drive.file`，讓腳本只管理它建立或開啟過的檔案：

```powershell
gcloud --account=chris@berlin.com.tw auth application-default login `
  --scopes=https://www.googleapis.com/auth/drive.file,https://www.googleapis.com/auth/cloud-platform `
  --project=tokenusage-chris-20260709
```

上傳並分享給 Cloud Run service account：

```powershell
pwsh -ExecutionPolicy Bypass -File .\scripts\upload-drive-snapshot.ps1 `
  -GcloudAccount chris@berlin.com.tw `
  -ProjectId tokenusage-chris-20260709 `
  -ShareWithServiceAccount token-insights-drive@demoproject-dotnet.iam.gserviceaccount.com
```

腳本會把 Drive file ID 存在 `%LOCALAPPDATA%\TokenUsageInsights\drive-snapshot-file-id.txt`。之後排程重跑會更新同一個檔案，而不是每次建立新檔。
Session timeline 事件會另外上傳成獨立 JSON 檔，並記錄在 `%LOCALAPPDATA%\TokenUsageInsights\drive-session-events-index.json`。預設只重新檢查最近 2 天的 session events，以避免每次排程都重抓並重設所有舊 session 的 Drive 權限；若需要完整重刷，執行時加上 `-RefreshAllSessionEvents`。

若某個 assistant 有大量從未上傳的 session（例如 copilot 的歷史積壓），會讓整個 run 因逐筆上傳而超時、連核心 snapshot 都上傳不了。此時可用 `-SkipSessionEventAssistants` 跳過該 assistant 的 session event 上傳（其每日／每月／每年核心資料仍照常匯出）：

```powershell
pwsh -ExecutionPolicy Bypass -File .\scripts\upload-drive-snapshot.ps1 `
  -ExportFromApi -ApiUrl http://localhost:3003 `
  -SkipSessionEventAssistants copilot
```

注意：目前 session event index 只在整個 run 成功結束時才存檔，因此積壓過大導致 run 無法完成時，已上傳的進度不會被記住。要讓大量積壓能跨多次排程逐步補完，需改為「增量存檔（checkpoint）」。

若本機已安裝的 `token-usage-insights.exe` 還不是支援 `--export-snapshot` 的版本，腳本會自動改從正在執行的本機看板 API 匯出。預設 API 是 `http://localhost:3003`，也可以明確指定：

```powershell
pwsh -ExecutionPolicy Bypass -File .\scripts\upload-drive-snapshot.ps1 `
  -ExportFromApi `
  -ApiUrl http://localhost:3003
```

若已經有現成 snapshot 檔案，可以略過匯出直接上傳：

```powershell
pwsh -ExecutionPolicy Bypass -File .\scripts\upload-drive-snapshot.ps1 `
  -SkipExport `
  -SnapshotPath "C:\path\to\snapshot.json"
```

本機已設定的排程名稱：

```text
TokenUsageInsights Drive Snapshot Upload
```

目前排程每 30 分鐘執行一次，從 `http://localhost:3003` 匯出 snapshot 後上傳 Drive。因此本機看板服務需要在排程執行時可連線。

## 本機看板服務重啟（`scripts/restart-service.ps1`）

本機看板以 NSSM 裝成 Windows 服務 `TokenUsageInsights`；因服務開了不停機記錄檔輪替（`AppRotateOnline=1`，NSSM 2.24-101 已知會卡在 STOP_PENDING），一律用此腳本重啟，不要直接 `Restart-Service`：

```text
TokenUsageInsights-NightlyRestart（每天 01:00，Highest 權限）
```

腳本會先 `Disable-ScheduledTask` 暫停上傳排程（等目前執行中的那次結束，上限 40 分鐘）、停止服務（卡住時只結束本服務的 nssm 主進程，不動同機其他 NSSM 服務）、啟動並驗證 API，`finally` 一律恢復上傳排程。此腳本內的服務名稱、路徑、埠號為本機安裝專屬，跨機器需自行調整。

## Google Drive 權限

Cloud Run 讀 Drive 時用兩個 service account（`src/snapshot.rs` 的 `google_access_token`）：

| Service account | 角色 |
|---|---|
| `token-insights-run@demoproject-dotnet.iam.gserviceaccount.com` | Cloud Run 的執行身分（`--service-account`） |
| `token-insights-drive@demoproject-dotnet.iam.gserviceaccount.com` | 讀 Drive 用的身分（環境變數 `DRIVE_SERVICE_ACCOUNT_EMAIL`）；執行身分透過 IAM Credentials `generateAccessToken` 取得它的 `drive.readonly` token，因此執行身分必須有權替它產生 token |

1. 把 `snapshot.json` 與 session 事件檔分享給 `token-insights-drive@…`，權限給 Viewer（上傳腳本的 `-ShareWithServiceAccount` 預設就是這個帳號，會自動分享）。
2. 在 Cloud Run 所在的 project 啟用 Google Drive API 與 IAM Service Account Credentials API。

## 部署到 Cloud Run

### 首次部署

```bash
gcloud services enable run.googleapis.com artifactregistry.googleapis.com drive.googleapis.com iap.googleapis.com \
  --project demoproject-dotnet

gcloud run deploy token-usage-insights \
  --source <乾淨原始碼資料夾，見下方「重新部署」> \
  --project demoproject-dotnet \
  --region asia-east1 \
  --service-account token-insights-run@demoproject-dotnet.iam.gserviceaccount.com \
  --no-allow-unauthenticated \
  --iap \
  --max-instances 1 \
  --set-env-vars TOKEN_USAGE_INSIGHTS_DATA_SOURCE=snapshot,DRIVE_SNAPSHOT_FILE_ID=<Drive 檔案 ID>,DRIVE_TOKEN_SCOPE=https://www.googleapis.com/auth/drive.readonly,DRIVE_SERVICE_ACCOUNT_EMAIL=token-insights-drive@demoproject-dotnet.iam.gserviceaccount.com,TOKEN_USAGE_INSIGHTS_SNAPSHOT_REFRESH_SECONDS=300
```

`<Drive 檔案 ID>` 是本機 `%LOCALAPPDATA%\TokenUsageInsights\drive-snapshot-file-id.txt` 的內容。

部署完成後，只授權指定使用者讀取：

```bash
gcloud iap web add-iam-policy-binding \
  --member=user:chris@berlin.com.tw \
  --role=roles/iap.httpsResourceAccessor \
  --project demoproject-dotnet \
  --region=asia-east1 \
  --resource-type=cloud-run \
  --service=token-usage-insights
```

注意：`roles/iap.httpsResourceAccessor` 只給「透過 IAP 開網頁」的權限，不含 project 管理權限；有這個角色的帳號打得開看板，但不一定查得到 project 本身。

### 重新部署（換版）

repo 沒有 `.gcloudignore`，直接 `--source .` 會把工作區裡未追蹤的檔案一起上傳到 Cloud Build 的原始碼 bucket。改從指定 commit 匯出乾淨資料夾再部署：

```bash
git archive <commit> | tar -x -C <乾淨資料夾>

gcloud run deploy token-usage-insights \
  --source <乾淨資料夾> \
  --project demoproject-dotnet \
  --region asia-east1
```

對既有服務重新部署時只會換 image；環境變數、service account、`--max-instances`、IAP 設定都沿用上一個 revision。部署完用 `gcloud run services describe token-usage-insights --project demoproject-dotnet --region asia-east1` 確認新 revision 已承接 100% 流量。

### 回滾

```bash
gcloud run revisions list --service token-usage-insights --project demoproject-dotnet --region asia-east1
gcloud run services update-traffic token-usage-insights --to-revisions=<舊 revision>=100 \
  --project demoproject-dotnet --region asia-east1
```

回滾需要該 revision 的 image 還在 Artifact Registry，清理舊 image 時至少保留上一版。

## 成本控制

- Cloud Run 設定 `min-instances=0`，閒置時縮到 0；`max-instances=1`。
- Artifact Registry 只保留最近 1 到 2 個 image（線上版加上一版供回滾）。每次 `--source` 部署都會新增一份（約 37 MB），用 `gcloud artifacts docker images list asia-east1-docker.pkg.dev/demoproject-dotnet/cloud-run-source-deploy/token-usage-insights --include-tags` 對照 `gcloud run revisions list` 的 image digest，再以 digest 逐一刪除沒有流量的舊 image。
- 不開 Artifact Registry vulnerability scanning，避免掃描費。
- 設 Billing budget alert，例如 1 USD 與 5 USD。
- 若只給自己看，`TOKEN_USAGE_INSIGHTS_SNAPSHOT_REFRESH_SECONDS=300` 或更高即可。

## 本機測 snapshot 模式

```powershell
$env:TOKEN_USAGE_INSIGHTS_DATA_SOURCE = "snapshot"
$env:TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH = "G:\My Drive\TokenUsageInsights\snapshot.json"
$env:PORT = "3004"
.\token-usage-insights.exe
```

開啟：

```text
http://localhost:3004/?agent=claude&tab=monthly&date=2026-07
```
