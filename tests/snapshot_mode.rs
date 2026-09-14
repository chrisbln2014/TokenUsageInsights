use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const ASSISTANTS: [&str; 9] = [
    "antigravity",
    "claude",
    "codex",
    "copilot",
    "cursor",
    "grok",
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
    assert!(
        lines.iter().any(|line| line.contains("snapshot")),
        "{lines:?}"
    );
    assert!(
        !insights_dir_created,
        "snapshot 模式不可建立本機資料目錄（SQLite／更新日誌）: {lines:?}"
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
