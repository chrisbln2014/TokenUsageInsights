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
