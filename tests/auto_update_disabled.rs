use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

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

// 以標準安裝環境＋明確開啟自動更新啟動看板；網路代理指向關閉的埠，
// 即使閘門失效也只會寫出更新日誌、不會真的下載官方版
#[test]
fn standard_install_never_runs_background_update_check_even_when_enabled() {
    let root = unique_temp_dir("auto-update-disabled");
    let install_dir = root.join("install");
    let insights_dir = root.join("insights");
    let empty_sources = root.join("empty-sources");
    for dir in [&install_dir, &insights_dir, &empty_sources] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // 預先建立空 DB，避免 get_db_conn 把執行者家目錄下的舊版統一資料庫搬過來
    std::fs::File::create(insights_dir.join("token_usage_insights.db")).unwrap();
    std::fs::write(insights_dir.join("config.yaml"), "auto_update: true\n").unwrap();

    let exe_name = std::path::Path::new(env!("CARGO_BIN_EXE_token-usage-insights"))
        .file_name()
        .unwrap()
        .to_owned();
    let installed_exe = install_dir.join(&exe_name);
    std::fs::copy(env!("CARGO_BIN_EXE_token-usage-insights"), &installed_exe).unwrap();

    let blocked_proxy = "http://127.0.0.1:9";
    let mut command = Command::new(&installed_exe);
    command
        .current_dir(&install_dir)
        .env("TOKEN_USAGE_INSIGHTS_INSTALL_DIR", &install_dir)
        .env("INSIGHTS_DIR", &insights_dir)
        .env("TOKEN_USAGE_INSIGHTS_AUTO_UPDATE", "1")
        .env("TOKEN_USAGE_INSIGHTS_UPDATE_INTERVAL_HOURS", "1")
        .env("HTTPS_PROXY", blocked_proxy)
        .env("HTTP_PROXY", blocked_proxy)
        .env("ALL_PROXY", blocked_proxy)
        .env("HOST", "127.0.0.1")
        .env("PORT", "0")
        .env("CURSOR_STATE_DB", empty_sources.join("state.vscdb"))
        .env_remove("NO_PROXY")
        .env_remove("CI")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("_TOKEN_USAGE_INSIGHTS_RESTARTED")
        .env_remove("TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH")
        .env_remove("TOKEN_USAGE_INSIGHTS_DATA_SOURCE")
        .env_remove("DRIVE_SNAPSHOT_FILE_ID")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
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
    // 背景更新檢查在啟動 1 秒後執行；留足時間讓失效的閘門寫出日誌
    std::thread::sleep(Duration::from_secs(5));
    let _ = child.kill();
    let _ = child.wait();
    while let Ok(line) = rx.try_recv() {
        lines.push(line);
    }

    let detected_standard_install = install_dir.join(".server.pid").exists();
    let update_log = std::fs::read_to_string(insights_dir.join("update.log")).ok();
    let _ = std::fs::remove_dir_all(&root);

    assert!(started, "看板未啟動: {lines:?}");
    assert!(
        detected_standard_install,
        "測試前提失效：未被判定為標準安裝環境，無法驗證自動更新閘門: {lines:?}"
    );
    assert_eq!(
        update_log, None,
        "fork 版不可執行背景自動更新檢查: {lines:?}"
    );
}
