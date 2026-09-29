use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const ASSISTANTS: [&str; 10] = [
    "antigravity",
    "claude",
    "codex",
    "copilot",
    "cursor",
    "grok",
    "mcode",
    "muse",
    "omp",
    "pi",
];

fn unique_temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "insights-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

// 所有日誌來源指向空目錄，避免測試讀取執行者真實的 AI 工具日誌
fn isolated_command(root: &Path, insights_dir: &Path) -> Command {
    let empty_sources = root.join("empty-sources");
    std::fs::create_dir_all(&empty_sources).unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_token-usage-insights"));
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("INSIGHTS_DIR", insights_dir)
        .env("TOKEN_USAGE_INSIGHTS_AUTO_UPDATE", "0")
        .env("CURSOR_STATE_DB", empty_sources.join("state.vscdb"))
        .env("MCODE_STATE_DB", empty_sources.join("mcode-state.db"))
        .env_remove("TOKEN_USAGE_INSIGHTS_EXPORT_SNAPSHOT")
        .env_remove("TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH")
        .env_remove("TOKEN_USAGE_INSIGHTS_DATA_SOURCE")
        .env_remove("DRIVE_SNAPSHOT_FILE_ID");
    for variable in [
        "ANTIGRAVITY_DIR",
        "COPILOT_DIR",
        "COPILOT_APP_DIR",
        "CODEX_DIR",
        "CLAUDE_DIR",
        "CURSOR_DIR",
        "GROK_DIR",
        "PI_DIR",
        "OMP_DIR",
        "MUSE_DIR",
        "MCODE_DIR",
        "VSCODE_USER_DATA_DIR",
        "VSCODE_PORTABLE_DATA_DIR",
        "APPDATA",
    ] {
        command.env(variable, &empty_sources);
    }
    command
}

#[test]
fn snapshot_mode_serves_without_creating_local_database_or_update_log() {
    let root = unique_temp_dir("snapshot-mode");
    let insights_dir = root.join("insights");
    let mut child = isolated_command(&root, &insights_dir)
        .env(
            "TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH",
            root.join("snapshot.json"),
        )
        .env("HOST", "127.0.0.1")
        .env("PORT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut lines = Vec::new();
    let started = loop {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(line) => {
                let ready = line.contains("is running on");
                lines.push(line);
                if ready {
                    break true;
                }
            }
            Err(_) => break false,
        }
    };
    std::thread::sleep(Duration::from_millis(1500));
    let still_running = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    let insights_dir_created = insights_dir.exists();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "snapshot 模式未啟動: {lines:?}");
    assert!(still_running, "snapshot 模式啟動後不應自行結束: {lines:?}");
    // snapshot 模式必須走自有啟動訊息，不可改走會開瀏覽器的 browser::announce_dashboard
    assert!(
        lines
            .iter()
            .any(|line| line.contains("(snapshot) is running on")),
        "{lines:?}"
    );
    assert!(
        !insights_dir_created,
        "snapshot 模式不可建立本機資料目錄（SQLite／更新日誌）: {lines:?}"
    );
}

struct HttpResponse {
    status: u16,
    content_type: Option<String>,
    body: String,
}

fn http_get(port: u16, path: &str) -> Result<HttpResponse, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).map_err(|e| e.to_string())?;

    let mut parts = raw.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let body = parts.next().unwrap_or_default().to_string();
    let mut head_lines = head.lines();
    let status_line = head_lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("無法解析狀態列: {status_line}"))?;
    let content_type = head_lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-type")
            .then(|| value.trim().to_string())
    });
    Ok(HttpResponse {
        status,
        content_type,
        body,
    })
}

/// 啟動 snapshot 模式子行程並等待就緒；回傳 (Child, 是否就緒, 已收到的 stdout 行)
fn spawn_snapshot_server(
    root: &Path,
    insights_dir: &Path,
    snapshot_path: &Path,
    port: u16,
) -> (std::process::Child, bool, Vec<String>) {
    let mut child = isolated_command(root, insights_dir)
        .env("TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH", snapshot_path)
        .env("HOST", "127.0.0.1")
        .env("PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut lines = Vec::new();
    let started = loop {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(line) => {
                let ready = line.contains("is running on");
                lines.push(line);
                if ready {
                    break true;
                }
            }
            Err(_) => break false,
        }
    };
    (child, started, lines)
}

#[test]
fn session_details_404_uses_pending_sync_message_not_reupload_instruction() {
    let root = unique_temp_dir("session-events-404");
    let insights_dir = root.join("insights");
    std::fs::create_dir_all(&root).unwrap();
    let snapshot_path = root.join("snapshot.json");
    std::fs::write(
        &snapshot_path,
        r#"{
            "schema_version": 1,
            "generated_at": "2026-09-28T00:00:00Z",
            "source": "test",
            "assistants": {
                "claude": {
                    "dates": [],
                    "months": [],
                    "years": [],
                    "daily": {},
                    "monthly": {},
                    "yearly": {}
                }
            }
        }"#,
    )
    .unwrap();

    // 先本地綁定一個空閒 port 再釋放給子行程用，降低與其他服務搶埠的機率
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    let (mut child, started, lines) =
        spawn_snapshot_server(&root, &insights_dir, &snapshot_path, port);

    let response = if started {
        http_get(port, "/api/claude/session/missing-session")
    } else {
        Err(format!("snapshot 模式未啟動: {lines:?}"))
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "snapshot 模式未啟動: {lines:?}");
    let response = response.expect("HTTP 請求失敗");
    assert_eq!(response.status, 404, "body={}", response.body);
    let payload: serde_json::Value = serde_json::from_str(&response.body).expect(&response.body);
    assert_eq!(
        payload["error"],
        "此 Session 的事件檔尚未同步到 Google Drive，下次排程（約 30 分鐘內）會補上；若本機紀錄已刪除則無法補回。"
    );
}

#[test]
fn usage_monthly_yearly_endpoints_return_snapshot_fixture_json_unchanged() {
    let root = unique_temp_dir("raw-json-passthrough");
    let insights_dir = root.join("insights");
    std::fs::create_dir_all(&root).unwrap();
    let snapshot_path = root.join("snapshot.json");

    let daily_fixture = serde_json::json!({
        "date": "2026-07-09",
        "summary": { "total_tokens": 110 }
    });
    let monthly_fixture = serde_json::json!({ "month": "2026-07", "total_tokens": 220 });
    let yearly_fixture = serde_json::json!({ "year": "2026", "total_tokens": 330 });

    let snapshot = serde_json::json!({
        "schema_version": 1,
        "generated_at": "2026-09-28T00:00:00Z",
        "source": "test",
        "assistants": {
            "claude": {
                "dates": ["2026-07-09"],
                "months": ["2026-07"],
                "years": ["2026"],
                "daily": { "2026-07-09": daily_fixture },
                "monthly": { "2026-07": monthly_fixture },
                "yearly": { "2026": yearly_fixture }
            }
        }
    });
    std::fs::write(&snapshot_path, serde_json::to_string(&snapshot).unwrap()).unwrap();

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    let (mut child, started, lines) =
        spawn_snapshot_server(&root, &insights_dir, &snapshot_path, port);

    let responses = if started {
        Ok((
            http_get(port, "/api/claude/usage/2026-07-09"),
            http_get(port, "/api/claude/monthly/2026-07"),
            http_get(port, "/api/claude/yearly/2026"),
        ))
    } else {
        Err(format!("snapshot 模式未啟動: {lines:?}"))
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "snapshot 模式未啟動: {lines:?}");
    let (daily_response, monthly_response, yearly_response) = responses.unwrap();

    for (label, response, fixture) in [
        ("daily", daily_response, &daily_fixture),
        ("monthly", monthly_response, &monthly_fixture),
        ("yearly", yearly_response, &yearly_fixture),
    ] {
        let response = response.unwrap_or_else(|e| panic!("{label} 請求失敗: {e}"));
        assert_eq!(response.status, 200, "{label} body={}", response.body);
        let content_type = response
            .content_type
            .unwrap_or_else(|| panic!("{label} 缺少 Content-Type"));
        assert!(
            content_type.starts_with("application/json"),
            "{label} content-type={content_type}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&response.body)
            .unwrap_or_else(|e| panic!("{label} 解析失敗: {e}"));
        assert_eq!(
            &parsed, fixture,
            "{label} 回應內容應與 snapshot fixture 原始 JSON 相等"
        );
    }
}

// R8：查詢一個 snapshot 裡不存在的日期／月份／年份，必須回 404，不能變成 200
// （static/app.js 依賴 404 判斷「當日無資料」）。
#[test]
fn usage_monthly_yearly_endpoints_return_404_for_missing_period() {
    let root = unique_temp_dir("raw-json-missing-period");
    let insights_dir = root.join("insights");
    std::fs::create_dir_all(&root).unwrap();
    let snapshot_path = root.join("snapshot.json");

    let snapshot = serde_json::json!({
        "schema_version": 1,
        "generated_at": "2026-09-28T00:00:00Z",
        "source": "test",
        "assistants": {
            "claude": {
                "dates": ["2026-07-09"],
                "months": ["2026-07"],
                "years": ["2026"],
                "daily": { "2026-07-09": { "date": "2026-07-09" } },
                "monthly": { "2026-07": { "month": "2026-07" } },
                "yearly": { "2026": { "year": "2026" } }
            }
        }
    });
    std::fs::write(&snapshot_path, serde_json::to_string(&snapshot).unwrap()).unwrap();

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    let (mut child, started, lines) =
        spawn_snapshot_server(&root, &insights_dir, &snapshot_path, port);

    let responses = if started {
        Ok((
            http_get(port, "/api/claude/usage/2099-01-01"),
            http_get(port, "/api/claude/monthly/2099-01"),
            http_get(port, "/api/claude/yearly/2099"),
        ))
    } else {
        Err(format!("snapshot 模式未啟動: {lines:?}"))
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "snapshot 模式未啟動: {lines:?}");
    let (daily_response, monthly_response, yearly_response) = responses.unwrap();

    for (label, response) in [
        ("daily", daily_response),
        ("monthly", monthly_response),
        ("yearly", yearly_response),
    ] {
        let response = response.unwrap_or_else(|e| panic!("{label} 請求失敗: {e}"));
        assert_eq!(
            response.status, 404,
            "{label} 找不到資料時應回 404，body={}",
            response.body
        );
    }
}

/// 啟動 snapshot 模式子行程，可自訂額外環境變數（用於模擬「只設 Drive file id」與
/// 「本機檔案路徑＋Drive file id 同時設定」兩種模式），並回傳一個持續接收 stderr 行的
/// channel，供之後判斷 get_cached_snapshot() 是否真的查詢過 Drive 版本。
fn spawn_snapshot_server_with_env(
    root: &Path,
    insights_dir: &Path,
    port: u16,
    extra_env: &[(&str, &str)],
) -> (
    std::process::Child,
    bool,
    Vec<String>,
    mpsc::Receiver<String>,
) {
    let mut command = isolated_command(root, insights_dir);
    command
        .env("HOST", "127.0.0.1")
        .env("PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();

    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let stderr = child.stderr.take().unwrap();
    let (stderr_tx, stderr_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if stderr_tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut lines = Vec::new();
    let started = loop {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(line) => {
                let ready = line.contains("is running on");
                lines.push(line);
                if ready {
                    break true;
                }
            }
            Err(_) => break false,
        }
    };
    (child, started, lines, stderr_rx)
}

/// 在 `total` 時間預算內盡量收集已送達的 stderr 行；一旦等不到新的一行就提早結束，
/// 不然沒有更多輸出時仍會把時間預算等到底。
fn drain_stderr_for(rx: &mpsc::Receiver<String>, total: Duration) -> Vec<String> {
    let mut lines = Vec::new();
    let deadline = std::time::Instant::now() + total;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(line) => lines.push(line),
            Err(_) => break,
        }
    }
    lines
}

// R4（審查員回歸）：should_use_drive_version_check 的真值表測試本身沒有執行到
// get_cached_snapshot() 裡真正的 if/else 分派邏輯——審查員實測過，把那段 if 改成恆真/恆假、
// 或呼叫時把兩個環境變數判斷參數對調，純函式測試依然全綠。這裡改成真的啟動子行程、真的走
// HTTP 請求，讓 get_cached_snapshot() 的分派邏輯被執行到，靠 stderr 有沒有出現「查詢 Drive
// 版本失敗」的警告字串來判斷是否真的呼叫了 drive_file_version()。
//
// 情境 (a)：只設定 DRIVE_SNAPSHOT_FILE_ID（模擬 Cloud Run 的 Drive 模式），並用一個連不到
// 的 HTTPS_PROXY 保證查版本一定失敗——只要 stderr 出現查詢失敗的警告，就證明真的有嘗試查
// 版本，能抓到「if 被改成恆假」或「呼叫參數對調」。
#[test]
fn drive_only_mode_queries_drive_version_and_logs_failure_on_unreachable_proxy() {
    let root = unique_temp_dir("drive-only-mode");
    let insights_dir = root.join("insights");
    std::fs::create_dir_all(&root).unwrap();

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    let (mut child, started, lines, stderr_rx) = spawn_snapshot_server_with_env(
        &root,
        &insights_dir,
        port,
        &[
            ("DRIVE_SNAPSHOT_FILE_ID", "fake-file-id"),
            ("GOOGLE_ACCESS_TOKEN", "fake-token"),
            // 保證連不到，curl 大約 2 秒內就會因連線失敗而回錯（已在本機驗證過耗時）
            ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ],
    );

    let response = if started {
        http_get(port, "/api/claude/yearly/2026")
    } else {
        Err(format!("snapshot 模式未啟動: {lines:?}"))
    };
    // 查版本失敗後程式碼會繼續嘗試完整下載（同樣連不到），這裡不斷言 HTTP 狀態碼——
    // 重點只在於「是否真的嘗試查過版本」，等 curl 逾時/失敗後這個請求本身失敗與否都可接受。
    let _ = response;

    let stderr_lines = drain_stderr_for(&stderr_rx, Duration::from_secs(6));

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "snapshot 模式未啟動: {lines:?}");
    assert!(
        stderr_lines
            .iter()
            .any(|line| line.contains("查詢 Drive 檔案版本失敗，直接嘗試下載")),
        "只設定 DRIVE_SNAPSHOT_FILE_ID 時應嘗試查詢 Drive 版本，但 stderr 沒有出現查詢失敗的警告: {stderr_lines:?}"
    );
}

// 情境 (b)：同時設定 TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH 與 DRIVE_SNAPSHOT_FILE_ID（模擬本機
// 檔案模式、但環境裡殘留了 Drive 設定）。不設定 GOOGLE_ACCESS_TOKEN，因為根本不該去查 Drive。
// 斷言請求正常 200，且 stderr 完全沒有查詢 Drive 版本的警告——能抓到「if 被改成恆真」。
#[test]
fn snapshot_path_mode_skips_drive_version_check_even_with_drive_id_set() {
    let root = unique_temp_dir("path-plus-drive-mode");
    let insights_dir = root.join("insights");
    std::fs::create_dir_all(&root).unwrap();
    let snapshot_path = root.join("snapshot.json");
    let snapshot = serde_json::json!({
        "schema_version": 1,
        "generated_at": "2026-09-28T00:00:00Z",
        "source": "test",
        "assistants": {
            "claude": {
                "dates": [],
                "months": [],
                "years": ["2026"],
                "daily": {},
                "monthly": {},
                "yearly": { "2026": { "year": "2026", "total_tokens": 1 } }
            }
        }
    });
    std::fs::write(&snapshot_path, serde_json::to_string(&snapshot).unwrap()).unwrap();

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    let (mut child, started, lines, stderr_rx) = spawn_snapshot_server_with_env(
        &root,
        &insights_dir,
        port,
        &[
            (
                "TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH",
                snapshot_path.to_str().unwrap(),
            ),
            ("DRIVE_SNAPSHOT_FILE_ID", "fake-file-id"),
        ],
    );

    let response = if started {
        http_get(port, "/api/claude/yearly/2026")
    } else {
        Err(format!("snapshot 模式未啟動: {lines:?}"))
    };

    let stderr_lines = drain_stderr_for(&stderr_rx, Duration::from_secs(3));

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "snapshot 模式未啟動: {lines:?}");
    let response = response.expect("HTTP 請求失敗");
    assert_eq!(response.status, 200, "body={}", response.body);
    assert!(
        !stderr_lines
            .iter()
            .any(|line| line.contains("查詢 Drive 檔案版本失敗")),
        "同時設定本機檔案路徑與 Drive file id 時不應查詢 Drive 版本，但 stderr 出現了: {stderr_lines:?}"
    );
}

#[test]
fn export_snapshot_flag_writes_snapshot_for_every_assistant() {
    let root = unique_temp_dir("export-snapshot");
    let insights_dir = root.join("insights");
    std::fs::create_dir_all(&insights_dir).unwrap();
    // 預先建立空 DB，避免 get_db_conn 把執行者家目錄下的舊版統一資料庫搬過來
    std::fs::File::create(insights_dir.join("token_usage_insights.db")).unwrap();
    let output_path = root.join("out").join("snapshot.json");

    let result = isolated_command(&root, &insights_dir)
        .arg("--export-snapshot")
        .arg(&output_path)
        .output()
        .unwrap();
    let written = std::fs::read_to_string(&output_path);
    let _ = std::fs::remove_dir_all(&root);

    assert!(result.status.success(), "{result:?}");
    let snapshot: serde_json::Value = serde_json::from_str(&written.unwrap()).unwrap();
    assert_eq!(snapshot["schema_version"], 1);
    let mut assistants: Vec<&str> = snapshot["assistants"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assistants.sort_unstable();
    assert_eq!(assistants, ASSISTANTS);
    // 日誌來源全指向空目錄，若出現資料代表隔離失效、測試讀到了執行者的真實日誌
    for assistant in ASSISTANTS {
        let dates = snapshot["assistants"][assistant]["dates"]
            .as_array()
            .unwrap();
        assert!(dates.is_empty(), "{assistant} 讀到非隔離資料: {dates:?}");
    }
}

#[test]
fn export_snapshot_env_does_not_hijack_subcommands() {
    let root = unique_temp_dir("export-snapshot-env");
    let insights_dir = root.join("insights");
    let hijacked_output = root.join("hijacked.json");

    let result = isolated_command(&root, &insights_dir)
        .env("TOKEN_USAGE_INSIGHTS_EXPORT_SNAPSHOT", &hijacked_output)
        .arg("--help")
        .output()
        .unwrap();
    let hijacked = hijacked_output.exists();
    let insights_dir_created = insights_dir.exists();
    let _ = std::fs::remove_dir_all(&root);

    assert!(result.status.success(), "{result:?}");
    assert!(String::from_utf8_lossy(&result.stdout).contains("token-usage-insights"));
    assert!(!hijacked, "環境變數不可把 --help 劫持成 snapshot 匯出");
    assert!(!insights_dir_created);
}

// ---- 「全部 Agent」合併範圍（assistant=all）整合測試（PLAN.md 驗收條件 #5/#6/#7） ----
//
// fixture 說明見 tests/fixtures/README.md 的
// `all-scope-claude-codex-snapshot-slice.json` 段落：這是從本機正式 Cloud Run
// snapshot 匯出檔切下來的一段真實資料（claude／codex 兩個 assistant，
// 2026-08-06／2026-08／2026 三個期間），匯出時的版本早於這次合併改動，
// daily_breakdown／monthly_breakdown 天生就沒有巢狀的 agents 欄位——用真實產物
// 而非手刻 JSON，符合 machine-gate 政策對契約測試輸入的要求，同時天然涵蓋
// 「缺 agents 欄位的舊格式」這個相容性情境，不用另外偽造。

const FIXTURE_DATE: &str = "2026-08-06";
const FIXTURE_MONTH: &str = "2026-08";
const FIXTURE_YEAR: &str = "2026";
// 三份都確認過兩個 assistant 皆不存在，用來驗證 404 行為。
const MISSING_DATE: &str = "2099-01-01";
const MISSING_MONTH: &str = "2099-01";
const MISSING_YEAR: &str = "2099";

fn two_assistant_snapshot_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("all-scope-claude-codex-snapshot-slice.json")
}

fn get_json(port: u16, path: &str) -> (u16, serde_json::Value) {
    let response = http_get(port, path).unwrap_or_else(|e| panic!("{path} 請求失敗: {e}"));
    let value: serde_json::Value = serde_json::from_str(&response.body)
        .unwrap_or_else(|e| panic!("{path} 回應解析失敗: {e}, body={}", response.body));
    (response.status, value)
}

fn as_string_vec(value: &serde_json::Value, field: &str) -> Vec<String> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} 不是陣列: {value}"))
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect()
}

/// 對兩個字串陣列取聯集、去重，並依 `src/snapshot.rs::union_desc` 的慣例排序成遞減，
/// 用來跟 `/api/all/...` 的回應比對——期望值是從同一份真實 fixture 的兩個單一
/// assistant 回應「算」出來的，不是憑空猜的常數。
fn union_desc_for_test(a: &[String], b: &[String]) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = a.iter().cloned().collect();
    set.extend(b.iter().cloned());
    set.into_iter().rev().collect()
}

/// 逐欄位比對「兩個單一 assistant 回應的 `summary.<field>` 相加」是否等於
/// 「合併回應的 `summary.<field>`」，數值欄位取自真實 fixture，不是手算的常數。
fn assert_summary_fields_sum(
    merged: &serde_json::Value,
    claude: &serde_json::Value,
    codex: &serde_json::Value,
    label: &str,
) {
    for field in [
        "total_sessions",
        "total_tokens",
        "total_input_tokens",
        "total_output_tokens",
        "total_cache_read_tokens",
        "total_cache_write_tokens",
        "total_reasoning_tokens",
        "total_duration_ms",
        "total_requests",
    ] {
        let claude_value = claude["summary"][field].as_u64().unwrap();
        let codex_value = codex["summary"][field].as_u64().unwrap();
        let merged_value = merged["summary"][field].as_u64().unwrap();
        assert_eq!(
            merged_value,
            claude_value + codex_value,
            "{label}.summary.{field}"
        );
    }
    let claude_cost = claude["summary"]["total_cost_usd"].as_f64().unwrap();
    let codex_cost = codex["summary"]["total_cost_usd"].as_f64().unwrap();
    let merged_cost = merged["summary"]["total_cost_usd"].as_f64().unwrap();
    assert!(
        (merged_cost - (claude_cost + codex_cost)).abs() < 1e-6,
        "{label}.summary.total_cost_usd: merged={merged_cost} expected={}",
        claude_cost + codex_cost
    );
}

/// 比對 `agent_breakdown` 的兩個 assistant 鍵，數值必須「重新從各自頂層 summary 建構」，
/// 而不是沿用來源回應裡巢狀的 agent_breakdown（PLAN.md 設計說明的「不信任巢狀既有值」）。
fn assert_agent_breakdown_matches_summaries(
    merged: &serde_json::Value,
    claude: &serde_json::Value,
    codex: &serde_json::Value,
) {
    let agent_breakdown = merged["agent_breakdown"].as_object().unwrap();
    assert_eq!(agent_breakdown.len(), 2, "{agent_breakdown:?}");
    for (assistant, source) in [("claude", claude), ("codex", codex)] {
        for field in [
            "total_tokens",
            "total_input_tokens",
            "total_output_tokens",
            "total_cache_read_tokens",
            "total_reasoning_tokens",
            "total_sessions",
        ] {
            assert_eq!(
                agent_breakdown[assistant][field], source["summary"][field],
                "agent_breakdown.{assistant}.{field}"
            );
        }
    }
}

#[test]
fn all_scope_dates_months_years_return_deduped_union_sorted_desc() {
    let root = unique_temp_dir("all-scope-lists");
    let insights_dir = root.join("insights");
    std::fs::create_dir_all(&root).unwrap();
    let snapshot_path = root.join("snapshot.json");
    std::fs::copy(two_assistant_snapshot_fixture_path(), &snapshot_path).unwrap();

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let (mut child, started, lines) =
        spawn_snapshot_server(&root, &insights_dir, &snapshot_path, port);

    let result = if started {
        Ok((
            get_json(port, "/api/claude/dates"),
            get_json(port, "/api/codex/dates"),
            get_json(port, "/api/all/dates"),
            get_json(port, "/api/claude/months"),
            get_json(port, "/api/codex/months"),
            get_json(port, "/api/all/months"),
            get_json(port, "/api/claude/years"),
            get_json(port, "/api/codex/years"),
            get_json(port, "/api/all/years"),
        ))
    } else {
        Err(format!("snapshot 模式未啟動: {lines:?}"))
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "snapshot 模式未啟動: {lines:?}");
    let (
        (claude_dates_status, claude_dates),
        (codex_dates_status, codex_dates),
        (all_dates_status, all_dates),
        (claude_months_status, claude_months),
        (codex_months_status, codex_months),
        (all_months_status, all_months),
        (claude_years_status, claude_years),
        (codex_years_status, codex_years),
        (all_years_status, all_years),
    ) = result.unwrap();

    for status in [
        claude_dates_status,
        codex_dates_status,
        all_dates_status,
        claude_months_status,
        codex_months_status,
        all_months_status,
        claude_years_status,
        codex_years_status,
        all_years_status,
    ] {
        assert_eq!(status, 200);
    }

    let expected_dates = union_desc_for_test(
        &as_string_vec(&claude_dates, "dates"),
        &as_string_vec(&codex_dates, "dates"),
    );
    assert!(!expected_dates.is_empty());
    assert!(expected_dates.contains(&FIXTURE_DATE.to_string()));
    assert_eq!(as_string_vec(&all_dates, "dates"), expected_dates);

    let expected_months = union_desc_for_test(
        &as_string_vec(&claude_months, "months"),
        &as_string_vec(&codex_months, "months"),
    );
    assert!(expected_months.contains(&FIXTURE_MONTH.to_string()));
    assert_eq!(as_string_vec(&all_months, "months"), expected_months);

    let expected_years = union_desc_for_test(
        &as_string_vec(&claude_years, "years"),
        &as_string_vec(&codex_years, "years"),
    );
    assert!(expected_years.contains(&FIXTURE_YEAR.to_string()));
    assert_eq!(as_string_vec(&all_years, "years"), expected_years);
}

#[test]
fn all_scope_usage_monthly_yearly_merge_totals_match_real_per_assistant_data() {
    let root = unique_temp_dir("all-scope-merge");
    let insights_dir = root.join("insights");
    std::fs::create_dir_all(&root).unwrap();
    let snapshot_path = root.join("snapshot.json");
    std::fs::copy(two_assistant_snapshot_fixture_path(), &snapshot_path).unwrap();

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let (mut child, started, lines) =
        spawn_snapshot_server(&root, &insights_dir, &snapshot_path, port);

    let result = if started {
        Ok((
            get_json(port, &format!("/api/claude/usage/{FIXTURE_DATE}")),
            get_json(port, &format!("/api/codex/usage/{FIXTURE_DATE}")),
            get_json(port, &format!("/api/all/usage/{FIXTURE_DATE}")),
            get_json(port, &format!("/api/all/usage/{MISSING_DATE}")),
            get_json(port, &format!("/api/claude/monthly/{FIXTURE_MONTH}")),
            get_json(port, &format!("/api/codex/monthly/{FIXTURE_MONTH}")),
            get_json(port, &format!("/api/all/monthly/{FIXTURE_MONTH}")),
            get_json(port, &format!("/api/all/monthly/{MISSING_MONTH}")),
            get_json(port, &format!("/api/claude/yearly/{FIXTURE_YEAR}")),
            get_json(port, &format!("/api/codex/yearly/{FIXTURE_YEAR}")),
            get_json(port, &format!("/api/all/yearly/{FIXTURE_YEAR}")),
            get_json(port, &format!("/api/all/yearly/{MISSING_YEAR}")),
        ))
    } else {
        Err(format!("snapshot 模式未啟動: {lines:?}"))
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "snapshot 模式未啟動: {lines:?}");
    let (
        (usage_claude_status, usage_claude),
        (usage_codex_status, usage_codex),
        (usage_all_status, usage_all),
        (usage_missing_status, usage_missing),
        (monthly_claude_status, monthly_claude),
        (monthly_codex_status, monthly_codex),
        (monthly_all_status, monthly_all),
        (monthly_missing_status, monthly_missing),
        (yearly_claude_status, yearly_claude),
        (yearly_codex_status, yearly_codex),
        (yearly_all_status, yearly_all),
        (yearly_missing_status, yearly_missing),
    ) = result.unwrap();

    for status in [
        usage_claude_status,
        usage_codex_status,
        monthly_claude_status,
        monthly_codex_status,
        yearly_claude_status,
        yearly_codex_status,
    ] {
        assert_eq!(status, 200, "單一 assistant 讀真實 fixture 應該一定成功");
    }

    // ---- daily：加總正確，且 sessions／raw_entries 保留各自真實的來源 assistant_type ----
    assert_eq!(usage_all_status, 200, "{usage_all}");
    assert_summary_fields_sum(&usage_all, &usage_claude, &usage_codex, "usage");

    let mut expected_session_types: Vec<String> = usage_claude["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .chain(usage_codex["sessions"].as_array().unwrap())
        .map(|s| s["assistant_type"].as_str().unwrap().to_string())
        .collect();
    expected_session_types.sort();
    let mut actual_session_types: Vec<String> = usage_all["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["assistant_type"].as_str().unwrap().to_string())
        .collect();
    actual_session_types.sort();
    assert_eq!(actual_session_types, expected_session_types);

    // raw_entries 的 assistant_type 是這次應修的重點：合併邏輯必須保留每一筆真實的
    // 來源標記，不能被覆寫成單一值（例如 "all"）——先前的測試只斷言 len()，這裡改成
    // 逐筆比對，且明確要求集合裡同時出現 "claude" 與 "codex" 兩種真實值。
    let mut expected_raw_types: Vec<String> = usage_claude["raw_entries"]
        .as_array()
        .unwrap()
        .iter()
        .chain(usage_codex["raw_entries"].as_array().unwrap())
        .map(|e| e["assistant_type"].as_str().unwrap().to_string())
        .collect();
    expected_raw_types.sort();
    let mut actual_raw_types: Vec<String> = usage_all["raw_entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["assistant_type"].as_str().unwrap().to_string())
        .collect();
    actual_raw_types.sort();
    assert_eq!(
        actual_raw_types, expected_raw_types,
        "合併後 raw_entries 的 assistant_type 必須逐筆保留真實來源，不能被覆寫成同一個值"
    );
    assert!(
        actual_raw_types.contains(&"claude".to_string())
            && actual_raw_types.contains(&"codex".to_string()),
        "{actual_raw_types:?}"
    );

    // Fix 2：home_dir 要用真實值（來源之一的 home_dir），不是自己編的佔位字串
    let claude_home_dir = usage_claude["home_dir"].as_str().unwrap();
    let merged_home_dir = usage_all["home_dir"].as_str().unwrap();
    assert_eq!(merged_home_dir, claude_home_dir);
    assert!(
        !merged_home_dir.contains("全部 Agent") && !merged_home_dir.contains("Google Drive"),
        "home_dir 不應該是佔位字串: {merged_home_dir}"
    );

    assert_eq!(usage_missing_status, 404, "{usage_missing}");
    assert_eq!(usage_missing["error"], "找不到該日期的使用量資料。");

    // ---- monthly：真實舊格式（daily_breakdown 項目天生缺 agents）驗證優雅降級不 500 ----
    assert_eq!(
        monthly_all_status, 200,
        "拿掉 #[serde(default)] 或合併邏輯壞掉都會讓這裡變 500: {monthly_all}"
    );
    assert_summary_fields_sum(&monthly_all, &monthly_claude, &monthly_codex, "monthly");
    assert_agent_breakdown_matches_summaries(&monthly_all, &monthly_claude, &monthly_codex);

    let claude_day = monthly_claude["daily_breakdown"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["date"] == FIXTURE_DATE)
        .unwrap();
    let codex_day = monthly_codex["daily_breakdown"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["date"] == FIXTURE_DATE)
        .unwrap();
    // fixture 前提：真實舊格式的來源資料本來就沒有巢狀 agents 欄位（見 fixture README）。
    assert!(
        claude_day.as_object().unwrap().get("agents").is_none(),
        "fixture 前提破壞：來源資料不該有 agents 欄位，{claude_day}"
    );
    let merged_day = monthly_all["daily_breakdown"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["date"] == FIXTURE_DATE)
        .unwrap();
    assert_eq!(
        merged_day["total_tokens"].as_u64().unwrap(),
        claude_day["total_tokens"].as_u64().unwrap() + codex_day["total_tokens"].as_u64().unwrap()
    );
    let merged_day_agents = merged_day["agents"].as_object().unwrap();
    assert_eq!(merged_day_agents.len(), 2);
    assert_eq!(
        merged_day_agents["claude"]["total_tokens"],
        claude_day["total_tokens"]
    );
    assert_eq!(
        merged_day_agents["codex"]["total_tokens"],
        codex_day["total_tokens"]
    );

    assert_eq!(monthly_missing_status, 404, "{monthly_missing}");
    assert_eq!(monthly_missing["error"], "找不到該月份的使用量資料。");

    // ---- yearly：同樣驗證真實舊格式（monthly_breakdown 項目天生缺 agents）優雅降級 ----
    assert_eq!(
        yearly_all_status, 200,
        "拿掉 #[serde(default)] 或合併邏輯壞掉都會讓這裡變 500: {yearly_all}"
    );
    assert_summary_fields_sum(&yearly_all, &yearly_claude, &yearly_codex, "yearly");
    assert_agent_breakdown_matches_summaries(&yearly_all, &yearly_claude, &yearly_codex);

    let claude_month = yearly_claude["monthly_breakdown"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["month"] == FIXTURE_MONTH)
        .unwrap();
    let codex_month = yearly_codex["monthly_breakdown"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["month"] == FIXTURE_MONTH)
        .unwrap();
    assert!(
        claude_month.as_object().unwrap().get("agents").is_none(),
        "fixture 前提破壞：來源資料不該有 agents 欄位，{claude_month}"
    );
    let merged_month = yearly_all["monthly_breakdown"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["month"] == FIXTURE_MONTH)
        .unwrap();
    assert_eq!(
        merged_month["total_tokens"].as_u64().unwrap(),
        claude_month["total_tokens"].as_u64().unwrap()
            + codex_month["total_tokens"].as_u64().unwrap()
    );
    let merged_month_agents = merged_month["agents"].as_object().unwrap();
    assert_eq!(merged_month_agents.len(), 2);
    assert_eq!(
        merged_month_agents["claude"]["total_tokens"],
        claude_month["total_tokens"]
    );
    assert_eq!(
        merged_month_agents["codex"]["total_tokens"],
        codex_month["total_tokens"]
    );

    assert_eq!(yearly_missing_status, 404, "{yearly_missing}");
    assert_eq!(yearly_missing["error"], "找不到該年份的使用量資料。");
}

// PLAN.md 驗收條件 #7：session / rate-limit / model-sessions 三個需要明確單一來源的端點，
// 帶 assistant=all 時仍須維持既有拒絕行為，不因這次放寬 dates/usage/monthly/yearly 而被誤放寬。
#[test]
fn all_scope_session_rate_limit_and_model_sessions_stay_rejected() {
    let root = unique_temp_dir("all-scope-rejected");
    let insights_dir = root.join("insights");
    std::fs::create_dir_all(&root).unwrap();
    let snapshot_path = root.join("snapshot.json");
    std::fs::copy(two_assistant_snapshot_fixture_path(), &snapshot_path).unwrap();

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let (mut child, started, lines) =
        spawn_snapshot_server(&root, &insights_dir, &snapshot_path, port);

    let result = if started {
        Ok((
            http_get(port, "/api/all/session/some-session"),
            http_get(port, "/api/all/rate-limit"),
            http_get(port, "/api/all/model-sessions"),
        ))
    } else {
        Err(format!("snapshot 模式未啟動: {lines:?}"))
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "snapshot 模式未啟動: {lines:?}");
    let (session_response, rate_limit_response, model_sessions_response) = result.unwrap();

    let session_response = session_response.unwrap();
    assert_eq!(
        session_response.status, 400,
        "body={}",
        session_response.body
    );

    let rate_limit_response = rate_limit_response.unwrap();
    assert_eq!(
        rate_limit_response.status, 400,
        "body={}",
        rate_limit_response.body
    );

    // model-sessions 路由固定指向 unsupported_in_snapshot_mode（501），跟 assistant 參數無關。
    let model_sessions_response = model_sessions_response.unwrap();
    assert_eq!(
        model_sessions_response.status, 501,
        "body={}",
        model_sessions_response.body
    );
}

// 背景說明的相容性清單（非 PLAN.md 獨立編號驗收條件）：setup-info 也在 upstream
// CHANGELOG 的相容性清單裡，一併確認放寬後仍能正常回應。
#[test]
fn all_scope_setup_info_still_responds_ok() {
    let root = unique_temp_dir("all-scope-setup-info");
    let insights_dir = root.join("insights");
    std::fs::create_dir_all(&root).unwrap();
    let snapshot_path = root.join("snapshot.json");
    std::fs::copy(two_assistant_snapshot_fixture_path(), &snapshot_path).unwrap();

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let (mut child, started, lines) =
        spawn_snapshot_server(&root, &insights_dir, &snapshot_path, port);

    let response = if started {
        http_get(port, "/api/all/setup-info")
    } else {
        Err(format!("snapshot 模式未啟動: {lines:?}"))
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "snapshot 模式未啟動: {lines:?}");
    let response = response.unwrap();
    assert_eq!(response.status, 200, "body={}", response.body);
}
