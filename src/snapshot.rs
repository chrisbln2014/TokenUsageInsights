use axum::{
    extract::Path as AxumPath,
    http::{header::CONTENT_TYPE, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{value::RawValue, Value};
use std::{
    collections::HashMap,
    fs,
    future::Future,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

use crate::handlers::{
    self, normalize_assistant_name, DateListResponse, MonthListResponse, YearListResponse,
};

const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const DEFAULT_REFRESH_SECONDS: u64 = 300;
const ASSISTANTS: [&str; 10] = [
    "antigravity",
    "copilot",
    "codex",
    "claude",
    "cursor",
    "grok",
    "pi",
    "omp",
    "muse",
    "mcode",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSnapshot {
    pub schema_version: u32,
    pub generated_at: String,
    pub source: String,
    pub assistants: HashMap<String, AssistantSnapshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssistantSnapshot {
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub dates: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub months: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub years: Vec<String>,
    pub daily: HashMap<String, Box<RawValue>>,
    pub monthly: HashMap<String, Box<RawValue>>,
    pub yearly: HashMap<String, Box<RawValue>>,
    #[serde(default)]
    pub session_events: HashMap<String, SessionEventRef>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionEventRef {
    pub drive_file_id: String,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub uploaded_at: Option<String>,
}

impl DashboardSnapshot {
    pub fn lookup_assistant(&self, assistant: &str) -> Option<&AssistantSnapshot> {
        self.assistants.get(&normalize_assistant_name(assistant))
    }

    pub fn lookup_daily(&self, assistant: &str, date: &str) -> Option<&RawValue> {
        self.lookup_assistant(assistant)?
            .daily
            .get(date)
            .map(Box::as_ref)
    }

    pub fn lookup_monthly(&self, assistant: &str, year_month: &str) -> Option<&RawValue> {
        self.lookup_assistant(assistant)?
            .monthly
            .get(year_month)
            .map(Box::as_ref)
    }

    pub fn lookup_yearly(&self, assistant: &str, year: &str) -> Option<&RawValue> {
        self.lookup_assistant(assistant)?
            .yearly
            .get(year)
            .map(Box::as_ref)
    }

    pub fn lookup_session_event(
        &self,
        assistant: &str,
        session_id: &str,
    ) -> Option<&SessionEventRef> {
        self.lookup_assistant(assistant)?
            .session_events
            .get(session_id)
    }
}

fn deserialize_string_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<Option<String>>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .flatten()
        .filter(|value| !value.trim().is_empty())
        .collect())
}

struct CachedSnapshot {
    loaded_at: Instant,
    snapshot: Arc<DashboardSnapshot>,
    /// 僅 Drive 模式會填值；本機檔案模式維持每次到期就重新載入，不比對版本
    version: Option<String>,
}

static SNAPSHOT_CACHE: OnceLock<Mutex<Option<CachedSnapshot>>> = OnceLock::new();

pub fn snapshot_mode_enabled() -> bool {
    std::env::var("TOKEN_USAGE_INSIGHTS_DATA_SOURCE")
        .map(|value| value.eq_ignore_ascii_case("snapshot"))
        .unwrap_or(false)
        || env_var_is_set("TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH")
        || env_var_is_set("DRIVE_SNAPSHOT_FILE_ID")
}

fn is_safe_session_id(session_id: &str) -> bool {
    if session_id.is_empty() || session_id.len() > 128 {
        return false;
    }

    if session_id == "." || session_id == ".." {
        return false;
    }

    session_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}

pub fn export_snapshot_path_from_args() -> Option<PathBuf> {
    export_snapshot_path(
        &std::env::args().collect::<Vec<_>>(),
        std::env::var("TOKEN_USAGE_INSIGHTS_EXPORT_SNAPSHOT").ok(),
    )
}

// 只接受第一個參數位置，避免搶走 upstream 子命令（export/import/update）自己的參數
fn export_snapshot_path(args: &[String], env_value: Option<String>) -> Option<PathBuf> {
    let is_usable = |value: &str| !value.trim().is_empty() && !value.starts_with("--");

    match args.get(1).map(String::as_str) {
        Some("--export-snapshot") if args.len() == 3 => args
            .get(2)
            .filter(|value| is_usable(value))
            .map(PathBuf::from),
        Some(_) => None,
        None => env_value
            .filter(|value| is_usable(value))
            .map(PathBuf::from),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotView {
    Dates,
    Months,
    Years,
    Daily,
    Monthly,
    Yearly,
}

/// 以 fetch 取得各 API 回應組成 snapshot；fetch 回 Ok(None) 代表該期間無資料（404）
pub async fn build_snapshot_with<F, Fut>(
    assistants: &[&str],
    fetch: F,
) -> Result<DashboardSnapshot, String>
where
    F: Fn(String, SnapshotView, String) -> Fut,
    Fut: Future<Output = Result<Option<Value>, String>>,
{
    let mut assistant_snapshots = HashMap::new();

    for assistant in assistants {
        let assistant = normalize_assistant_name(assistant);
        let dates = fetch_string_list(&fetch, &assistant, SnapshotView::Dates, "dates").await?;
        let months = fetch_string_list(&fetch, &assistant, SnapshotView::Months, "months").await?;
        let years = fetch_string_list(&fetch, &assistant, SnapshotView::Years, "years").await?;

        let mut daily = fetch_view_map(&fetch, &assistant, SnapshotView::Daily, &dates).await?;
        for value in daily.values_mut() {
            strip_transcript_paths(value);
        }
        let daily = to_raw_map(daily)?;
        let monthly =
            to_raw_map(fetch_view_map(&fetch, &assistant, SnapshotView::Monthly, &months).await?)?;
        let yearly =
            to_raw_map(fetch_view_map(&fetch, &assistant, SnapshotView::Yearly, &years).await?)?;

        assistant_snapshots.insert(
            assistant,
            AssistantSnapshot {
                dates,
                months,
                years,
                daily,
                monthly,
                yearly,
                session_events: HashMap::new(),
            },
        );
    }

    Ok(DashboardSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        generated_at: chrono::Utc::now().to_rfc3339(),
        source: "sqlite-export".to_string(),
        assistants: assistant_snapshots,
    })
}

async fn fetch_string_list<F, Fut>(
    fetch: &F,
    assistant: &str,
    view: SnapshotView,
    field: &str,
) -> Result<Vec<String>, String>
where
    F: Fn(String, SnapshotView, String) -> Fut,
    Fut: Future<Output = Result<Option<Value>, String>>,
{
    let Some(value) = fetch(assistant.to_string(), view, String::new()).await? else {
        return Ok(Vec::new());
    };
    Ok(value
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|item| !item.trim().is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default())
}

async fn fetch_view_map<F, Fut>(
    fetch: &F,
    assistant: &str,
    view: SnapshotView,
    keys: &[String],
) -> Result<HashMap<String, Value>, String>
where
    F: Fn(String, SnapshotView, String) -> Fut,
    Fut: Future<Output = Result<Option<Value>, String>>,
{
    let mut map = HashMap::new();
    for key in keys {
        if let Some(value) = fetch(assistant.to_string(), view, key.clone()).await? {
            map.insert(key.clone(), value);
        }
    }
    Ok(map)
}

/// 把解析過的 Value 轉成原始 JSON（Box<RawValue>），回應時可直接送出文字，不必再解析或整份複製
fn to_raw_map(map: HashMap<String, Value>) -> Result<HashMap<String, Box<RawValue>>, String> {
    map.into_iter()
        .map(|(key, value)| {
            let raw = serde_json::value::to_raw_value(&value)
                .map_err(|e| format!("轉換為 RawValue 失敗 ({key}): {e}"))?;
            Ok((key, raw))
        })
        .collect()
}

fn strip_transcript_paths(daily: &mut Value) {
    if let Some(entries) = daily.get_mut("raw_entries").and_then(Value::as_array_mut) {
        for entry in entries {
            if let Some(object) = entry.as_object_mut() {
                object.insert("transcript_path".to_string(), Value::Null);
            }
        }
    }
}

/// 直接呼叫本機看板的 handler，確保 snapshot 內容與本機 API 回應完全一致
async fn fetch_from_dashboard_handlers(
    assistant: String,
    view: SnapshotView,
    key: String,
) -> Result<Option<Value>, String> {
    let label = format!("{view:?} {assistant}/{key}");
    let response = match view {
        SnapshotView::Dates => handlers::get_available_dates(AxumPath(assistant))
            .await
            .into_response(),
        SnapshotView::Months => handlers::get_available_months(AxumPath(assistant))
            .await
            .into_response(),
        SnapshotView::Years => handlers::get_available_years(AxumPath(assistant))
            .await
            .into_response(),
        SnapshotView::Daily => handlers::get_usage_details(AxumPath((assistant, key)))
            .await
            .into_response(),
        SnapshotView::Monthly => handlers::get_monthly_details(AxumPath((assistant, key)))
            .await
            .into_response(),
        SnapshotView::Yearly => handlers::get_yearly_details(AxumPath((assistant, key)))
            .await
            .into_response(),
    };

    response_to_optional_json(&label, response).await
}

async fn response_to_optional_json(
    label: &str,
    response: axum::response::Response,
) -> Result<Option<Value>, String> {
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|e| format!("讀取 {label} 回應失敗: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "{label} 回應 {status}: {}",
            String::from_utf8_lossy(&body)
        ));
    }
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| format!("解析 {label} 回應失敗: {e}"))
}

pub async fn write_snapshot_file(path: &Path) -> Result<(), String> {
    let snapshot = build_snapshot_with(&ASSISTANTS, fetch_from_dashboard_handlers).await?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("建立 snapshot 目錄失敗: {e}"))?;
        }
    }
    let data = serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?;
    fs::write(path, data).map_err(|e| format!("寫入 snapshot 失敗: {e}"))
}

async fn load_snapshot_from_env() -> Result<DashboardSnapshot, String> {
    if let Ok(path) = std::env::var("TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH") {
        let data = fs::read_to_string(&path)
            .map_err(|e| format!("讀取 snapshot 檔案失敗 ({path}): {e}"))?;
        return serde_json::from_str(&data).map_err(|e| format!("解析 snapshot JSON 失敗: {e}"));
    }

    if let Ok(file_id) = std::env::var("DRIVE_SNAPSHOT_FILE_ID") {
        let data = fetch_drive_file(&file_id).await?;
        return serde_json::from_str(&data)
            .map_err(|e| format!("解析 Drive snapshot JSON 失敗: {e}"));
    }

    Err("未設定 TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH 或 DRIVE_SNAPSHOT_FILE_ID".to_string())
}

async fn get_cached_snapshot() -> Result<Arc<DashboardSnapshot>, String> {
    let refresh_seconds = std::env::var("TOKEN_USAGE_INSIGHTS_SNAPSHOT_REFRESH_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_REFRESH_SECONDS);
    let ttl = Duration::from_secs(refresh_seconds);
    let cache = SNAPSHOT_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().await;

    let download = || load_snapshot_from_env();

    let use_drive_version_check = should_use_drive_version_check(
        env_var_is_set("TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH"),
        env_var_is_set("DRIVE_SNAPSHOT_FILE_ID"),
    );

    if use_drive_version_check {
        let file_id = std::env::var("DRIVE_SNAPSHOT_FILE_ID").unwrap_or_default();
        let check_version = || async { drive_file_version(&file_id).await };
        refresh_cached_snapshot(&mut guard, ttl, &check_version, &download).await
    } else {
        // 本機檔案模式：維持現狀，時間到就重新載入（量測記憶體需要量到最糟情況）
        let no_version_check = || async { Ok::<Option<String>, String>(None) };
        refresh_cached_snapshot(&mut guard, ttl, &no_version_check, &download).await
    }
}

/// 是否該對 Drive 檔案做版本查詢：與 `load_snapshot_from_env` 相同的優先順序，本機檔案模式
/// 優先於 Drive 模式，兩者都設定時不啟用「查版本、沒變就不下載」，避免下載走檔案、查版本卻打
/// Drive 導致永遠沿用舊快取。抽成接受布林值的純函式（R4）以利測試，不直接讀真實環境變數。
fn should_use_drive_version_check(
    snapshot_path_env_is_set: bool,
    drive_file_id_env_is_set: bool,
) -> bool {
    !snapshot_path_env_is_set && drive_file_id_env_is_set
}

/// 刷新快取的核心邏輯，抽成可注入版本查詢／下載函式的純函式以利測試（不依賴全域快取或真實網路）。
/// `check_version` 回傳 `Ok(None)` 代表無法判斷版本（例如本機檔案模式），一律視為需要下載。
async fn refresh_cached_snapshot<V, VFut, D, DFut>(
    guard: &mut Option<CachedSnapshot>,
    ttl: Duration,
    check_version: &V,
    download: &D,
) -> Result<Arc<DashboardSnapshot>, String>
where
    V: Fn() -> VFut,
    VFut: Future<Output = Result<Option<String>, String>>,
    D: Fn() -> DFut,
    DFut: Future<Output = Result<DashboardSnapshot, String>>,
{
    if let Some(cached) = guard.as_ref() {
        if cached.loaded_at.elapsed() < ttl {
            return Ok(cached.snapshot.clone());
        }
    }

    let mut new_version: Option<String> = None;
    match check_version().await {
        Ok(version) => {
            new_version = version;
            let unchanged = matches!(
                (new_version.as_deref(), guard.as_ref().and_then(|c| c.version.as_deref())),
                (Some(new_v), Some(old_v)) if new_v == old_v
            );
            if unchanged {
                if let Some(cached) = guard.as_mut() {
                    cached.loaded_at = Instant::now();
                    return Ok(cached.snapshot.clone());
                }
            }
        }
        Err(err) => {
            // F1 修正：查版本失敗不能直接沿用快取——如果這個新請求在正式環境長期失敗
            // （但下載本身正常），會讓 Cloud Run 永遠停在第一次載入的舊資料。改成繼續往下
            // 嘗試完整下載；下載也失敗時，下面的 download() 失敗分支仍會沿用舊快取。
            eprintln!("⚠️ 查詢 Drive 檔案版本失敗，直接嘗試下載: {err}");
        }
    }

    match download().await {
        Ok(snapshot) => {
            let snapshot = Arc::new(snapshot);
            *guard = Some(CachedSnapshot {
                loaded_at: Instant::now(),
                snapshot: snapshot.clone(),
                version: new_version,
            });
            Ok(snapshot)
        }
        Err(err) => {
            if let Some(cached) = guard.as_ref() {
                eprintln!("⚠️ 重新載入 snapshot 失敗，使用快取資料: {err}");
                Ok(cached.snapshot.clone())
            } else {
                Err(err)
            }
        }
    }
}

/// 輕量查詢 Drive 檔案版本，不下載整份內容；用於刷新前判斷是否值得重新下載
async fn drive_file_version(file_id: &str) -> Result<Option<String>, String> {
    let token = google_access_token()?;
    let url =
        format!("https://www.googleapis.com/drive/v3/files/{file_id}?fields=version,modifiedTime");
    let body = run_curl(&[
        "-fsS",
        "--max-time",
        "20",
        "-H",
        &format!("Authorization: Bearer {token}"),
        &url,
    ])
    .map_err(|e| format!("查詢 Drive 檔案版本失敗 ({file_id}): {e}"))?;
    let payload: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let version = payload
        .get("version")
        .and_then(Value::as_str)
        .or_else(|| payload.get("modifiedTime").and_then(Value::as_str))
        .map(ToOwned::to_owned);
    Ok(version)
}

async fn fetch_drive_file(file_id: &str) -> Result<String, String> {
    let token = google_access_token()?;
    let url = format!("https://www.googleapis.com/drive/v3/files/{file_id}?alt=media");
    run_curl(&[
        "-fsS",
        "--max-time",
        "20",
        "-H",
        &format!("Authorization: Bearer {token}"),
        &url,
    ])
    .map_err(|e| format!("下載 Drive 檔案失敗 ({file_id}): {e}"))
}

fn google_access_token() -> Result<String, String> {
    if let Ok(token) = std::env::var("GOOGLE_ACCESS_TOKEN") {
        if !token.trim().is_empty() {
            return Ok(token);
        }
    }

    let scope = std::env::var("DRIVE_TOKEN_SCOPE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "https://www.googleapis.com/auth/drive.readonly".to_string());

    let token_source = std::env::var("DRIVE_TOKEN_SOURCE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "iam".to_string());
    if token_source.eq_ignore_ascii_case("metadata") {
        return metadata_access_token(Some(&scope));
    }

    iam_scoped_access_token(&scope)
}

fn iam_scoped_access_token(scope: &str) -> Result<String, String> {
    let bootstrap_token = metadata_access_token(None)?;
    let service_account_email = cloud_run_service_account_email()?;
    let request_body = serde_json::json!({
        "scope": [scope],
        "lifetime": "3600s"
    })
    .to_string();
    let url = format!(
        "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/{}:generateAccessToken",
        percent_encode(&service_account_email)
    );
    let response = run_curl(&[
        "-fsS",
        "--max-time",
        "20",
        "-X",
        "POST",
        "-H",
        &format!("Authorization: Bearer {bootstrap_token}"),
        "-H",
        "Content-Type: application/json",
        "-d",
        &request_body,
        &url,
    ])
    .map_err(|e| {
        format!("透過 IAM Credentials 取得 Drive scoped token 失敗 ({service_account_email}): {e}")
    })?;
    let payload: Value = serde_json::from_str(&response).map_err(|e| e.to_string())?;
    payload
        .get("accessToken")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| "IAM Credentials 回應沒有 accessToken".to_string())
}

fn cloud_run_service_account_email() -> Result<String, String> {
    if let Ok(email) = std::env::var("DRIVE_SERVICE_ACCOUNT_EMAIL") {
        if !email.trim().is_empty() {
            return Ok(email);
        }
    }

    let email = run_curl(&[
        "-fsS",
        "--max-time",
        "20",
        "-H",
        "Metadata-Flavor: Google",
        "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/email",
    ])
    .map_err(|e| format!("從 Cloud Run metadata server 取得 service account email 失敗: {e}"))?;

    let email = email.trim();
    if email.is_empty() {
        return Err("metadata server 回應的 service account email 為空".to_string());
    }

    Ok(email.to_string())
}

fn metadata_access_token(scope: Option<&str>) -> Result<String, String> {
    let metadata_url = match scope {
        Some(scope) => format!(
            "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token?scopes={}",
            percent_encode(scope)
        ),
        None => "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token".to_string(),
    };
    let metadata = run_curl(&[
        "-fsS",
        "--max-time",
        "20",
        "-H",
        "Metadata-Flavor: Google",
        &metadata_url,
    ])
    .map_err(|e| format!("從 Cloud Run metadata server 取得 token 失敗: {e}"))?;
    let payload: Value = serde_json::from_str(&metadata).map_err(|e| e.to_string())?;
    payload
        .get("access_token")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| "metadata server 回應沒有 access_token".to_string())
}

fn env_var_is_set(name: &str) -> bool {
    std::env::var(name)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn percent_encode(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn run_curl(args: &[&str]) -> Result<String, String> {
    let output = Command::new("curl")
        .args(args)
        .output()
        .map_err(|e| format!("執行 curl 失敗: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn unsupported_assistant_response() -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": "不支援的助理類型" })),
    )
        .into_response()
}

fn snapshot_error_response(err: String) -> axum::response::Response {
    eprintln!("❌ Snapshot API error: {err}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": err })),
    )
        .into_response()
}

fn not_found_response(message: &str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

/// 直接送出 snapshot 內保存的原始 JSON 文字，不解析、不複製整份 snapshot
fn raw_json_response(value: &RawValue) -> axum::response::Response {
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "application/json")],
        value.get().to_owned(),
    )
        .into_response()
}

pub async fn get_available_dates(AxumPath(assistant): AxumPath<String>) -> impl IntoResponse {
    let assistant = normalize_assistant_name(&assistant);
    if !ASSISTANTS.contains(&assistant.as_str()) {
        return unsupported_assistant_response();
    }
    match get_cached_snapshot().await {
        Ok(snapshot) => {
            let dates = snapshot
                .lookup_assistant(&assistant)
                .map(|item| item.dates.clone())
                .unwrap_or_default();
            Json(DateListResponse { dates }).into_response()
        }
        Err(err) => snapshot_error_response(err),
    }
}

pub async fn get_available_months(AxumPath(assistant): AxumPath<String>) -> impl IntoResponse {
    let assistant = normalize_assistant_name(&assistant);
    if !ASSISTANTS.contains(&assistant.as_str()) {
        return unsupported_assistant_response();
    }
    match get_cached_snapshot().await {
        Ok(snapshot) => {
            let months = snapshot
                .lookup_assistant(&assistant)
                .map(|item| item.months.clone())
                .unwrap_or_default();
            Json(MonthListResponse { months }).into_response()
        }
        Err(err) => snapshot_error_response(err),
    }
}

pub async fn get_available_years(AxumPath(assistant): AxumPath<String>) -> impl IntoResponse {
    let assistant = normalize_assistant_name(&assistant);
    if !ASSISTANTS.contains(&assistant.as_str()) {
        return unsupported_assistant_response();
    }
    match get_cached_snapshot().await {
        Ok(snapshot) => {
            let years = snapshot
                .lookup_assistant(&assistant)
                .map(|item| item.years.clone())
                .unwrap_or_default();
            Json(YearListResponse { years }).into_response()
        }
        Err(err) => snapshot_error_response(err),
    }
}

pub async fn get_usage_details(
    AxumPath((assistant, date)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let assistant = normalize_assistant_name(&assistant);
    if !ASSISTANTS.contains(&assistant.as_str()) {
        return unsupported_assistant_response();
    }
    match get_cached_snapshot().await {
        Ok(snapshot) => {
            if let Some(value) = snapshot.lookup_daily(&assistant, &date) {
                raw_json_response(value)
            } else {
                not_found_response("找不到該日期的使用量資料。")
            }
        }
        Err(err) => snapshot_error_response(err),
    }
}

pub async fn get_monthly_details(
    AxumPath((assistant, year_month)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let assistant = normalize_assistant_name(&assistant);
    if !ASSISTANTS.contains(&assistant.as_str()) {
        return unsupported_assistant_response();
    }
    match get_cached_snapshot().await {
        Ok(snapshot) => {
            if let Some(value) = snapshot.lookup_monthly(&assistant, &year_month) {
                raw_json_response(value)
            } else {
                not_found_response("找不到該月份的使用量資料。")
            }
        }
        Err(err) => snapshot_error_response(err),
    }
}

pub async fn get_yearly_details(
    AxumPath((assistant, year)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let assistant = normalize_assistant_name(&assistant);
    if !ASSISTANTS.contains(&assistant.as_str()) {
        return unsupported_assistant_response();
    }
    match get_cached_snapshot().await {
        Ok(snapshot) => {
            if let Some(value) = snapshot.lookup_yearly(&assistant, &year) {
                raw_json_response(value)
            } else {
                not_found_response("找不到該年份的使用量資料。")
            }
        }
        Err(err) => snapshot_error_response(err),
    }
}

pub async fn get_setup_info(AxumPath(assistant): AxumPath<String>) -> impl IntoResponse {
    let assistant = normalize_assistant_name(&assistant);
    if !ASSISTANTS.contains(&assistant.as_str()) {
        return unsupported_assistant_response();
    }

    Json(snapshot_setup_info()).into_response()
}

fn snapshot_setup_info() -> Value {
    let mut info = serde_json::Map::new();
    info.insert(
        "workspace_dir".to_string(),
        Value::from("Cloud Run snapshot mode"),
    );
    info.insert("home_dir".to_string(), Value::from("Google Drive snapshot"));
    for assistant in ASSISTANTS {
        info.insert(
            assistant.to_string(),
            serde_json::json!({ "dir_path": "snapshot", "exists": true, "script_path": "" }),
        );
    }
    Value::Object(info)
}

pub async fn trigger_manual_sync(AxumPath(assistant): AxumPath<String>) -> impl IntoResponse {
    let assistant = normalize_assistant_name(&assistant);
    if !ASSISTANTS.contains(&assistant.as_str()) {
        return unsupported_assistant_response();
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "disabled",
            "message": "Cloud Run snapshot mode is read-only. Update Google Drive snapshot from the local exporter."
        })),
    )
        .into_response()
}

pub async fn get_session_details(
    AxumPath((assistant, session_id)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let assistant = normalize_assistant_name(&assistant);
    if !ASSISTANTS.contains(&assistant.as_str()) {
        return unsupported_assistant_response();
    }

    if !is_safe_session_id(&session_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "非法的 session_id 格式。" })),
        )
            .into_response();
    }

    match get_cached_snapshot().await {
        Ok(snapshot) => {
            let Some(event_ref) = snapshot.lookup_session_event(&assistant, &session_id) else {
                return not_found_response(
                    "此 Session 的事件檔尚未同步到 Google Drive，下次排程（約 30 分鐘內）會補上；若本機紀錄已刪除則無法補回。",
                );
            };

            if event_ref.drive_file_id.trim().is_empty() {
                return not_found_response("Snapshot 中的 Session 事件檔 Drive file id 為空。");
            }

            let raw = match fetch_drive_file(&event_ref.drive_file_id).await {
                Ok(raw) => raw,
                Err(err) => return snapshot_error_response(err),
            };

            match serde_json::from_str::<Value>(&raw) {
                Ok(value) => Json(value).into_response(),
                Err(err) => snapshot_error_response(format!(
                    "Session 事件檔 JSON 解析失敗 ({}): {err}",
                    event_ref.drive_file_id
                )),
            }
        }
        Err(err) => snapshot_error_response(err),
    }
}

pub async fn unsupported_in_snapshot_mode() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "Cloud Run snapshot 模式為唯讀，不支援此功能（Session 搜尋、模型 Session 明細、匯出／匯入）。請在本機看板使用。"
        })),
    )
        .into_response()
}

pub async fn get_rate_limit(AxumPath(assistant): AxumPath<String>) -> impl IntoResponse {
    let assistant = normalize_assistant_name(&assistant);
    if !ASSISTANTS.contains(&assistant.as_str()) {
        return unsupported_assistant_response();
    }
    not_found_response("Snapshot 中沒有 Codex rate limit 資料。")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_export_assembles_api_responses_and_strips_transcript_paths() {
        let snapshot = build_snapshot_with(&["antigravity", "claude"], |assistant, view, key| async move {
            let is_antigravity = assistant == "antigravity";
            Ok(match (view, key.as_str()) {
                (SnapshotView::Dates, _) if is_antigravity => {
                    Some(serde_json::json!({ "dates": [null, "2026-07-09", ""] }))
                }
                (SnapshotView::Months, _) if is_antigravity => {
                    Some(serde_json::json!({ "months": ["2026-07"] }))
                }
                (SnapshotView::Years, _) if is_antigravity => {
                    Some(serde_json::json!({ "years": ["2026"] }))
                }
                (SnapshotView::Dates | SnapshotView::Months | SnapshotView::Years, _) => None,
                (SnapshotView::Daily, "2026-07-09") => Some(serde_json::json!({
                    "date": "2026-07-09",
                    "summary": { "total_tokens": 110 },
                    "raw_entries": [
                        { "session_id": "session-1", "transcript_path": "C:\\Users\\me\\.codex\\s.jsonl" }
                    ]
                })),
                (SnapshotView::Monthly, _) => None,
                (SnapshotView::Yearly, "2026") => Some(serde_json::json!({ "year": "2026" })),
                (view, key) => panic!("未預期的請求: {view:?} {assistant}/{key}"),
            })
        })
        .await
        .unwrap();

        assert_eq!(snapshot.schema_version, 1);
        let antigravity = snapshot.lookup_assistant("antigravity").unwrap();
        assert_eq!(antigravity.dates, vec!["2026-07-09"]);
        assert_eq!(antigravity.months, vec!["2026-07"]);

        let daily_raw = snapshot.lookup_daily("antigravity", "2026-07-09").unwrap();
        let daily: Value = serde_json::from_str(daily_raw.get()).unwrap();
        assert_eq!(daily["summary"]["total_tokens"], 110);
        assert_eq!(daily["raw_entries"][0]["session_id"], "session-1");
        assert!(daily["raw_entries"][0]["transcript_path"].is_null());

        assert!(snapshot.lookup_monthly("antigravity", "2026-07").is_none());
        let yearly_raw = snapshot.lookup_yearly("antigravity", "2026").unwrap();
        let yearly: Value = serde_json::from_str(yearly_raw.get()).unwrap();
        assert_eq!(yearly["year"], "2026");
        assert!(snapshot
            .lookup_assistant("claude")
            .unwrap()
            .dates
            .is_empty());
    }

    #[tokio::test]
    async fn snapshot_daily_monthly_yearly_round_trip_export_import_keeps_structure() {
        let snapshot = build_snapshot_with(&["claude"], |_, view, key| async move {
            Ok(match (view, key.as_str()) {
                (SnapshotView::Dates, _) => Some(serde_json::json!({ "dates": ["2026-07-09"] })),
                (SnapshotView::Months, _) => Some(serde_json::json!({ "months": ["2026-07"] })),
                (SnapshotView::Years, _) => Some(serde_json::json!({ "years": ["2026"] })),
                (SnapshotView::Daily, "2026-07-09") => Some(serde_json::json!({
                    "date": "2026-07-09",
                    "summary": { "total_tokens": 42 },
                    "raw_entries": [
                        { "session_id": "s1", "transcript_path": "C:\\Users\\me\\.codex\\s.jsonl" }
                    ]
                })),
                (SnapshotView::Monthly, "2026-07") => {
                    Some(serde_json::json!({ "month": "2026-07", "total_tokens": 42 }))
                }
                (SnapshotView::Yearly, "2026") => {
                    Some(serde_json::json!({ "year": "2026", "total_tokens": 42 }))
                }
                _ => None,
            })
        })
        .await
        .unwrap();

        // 匯出（序列化）再讀回（反序列化），結構必須不變
        let exported = serde_json::to_string(&snapshot).unwrap();
        let reloaded: DashboardSnapshot = serde_json::from_str(&exported).unwrap();

        let original_daily: Value =
            serde_json::from_str(snapshot.lookup_daily("claude", "2026-07-09").unwrap().get())
                .unwrap();
        let reloaded_daily: Value =
            serde_json::from_str(reloaded.lookup_daily("claude", "2026-07-09").unwrap().get())
                .unwrap();
        assert_eq!(original_daily, reloaded_daily);
        // transcript_path 清除的效果要在匯出再讀回之後仍然保留
        assert!(reloaded_daily["raw_entries"][0]["transcript_path"].is_null());

        let original_monthly: Value =
            serde_json::from_str(snapshot.lookup_monthly("claude", "2026-07").unwrap().get())
                .unwrap();
        let reloaded_monthly: Value =
            serde_json::from_str(reloaded.lookup_monthly("claude", "2026-07").unwrap().get())
                .unwrap();
        assert_eq!(original_monthly, reloaded_monthly);

        let original_yearly: Value =
            serde_json::from_str(snapshot.lookup_yearly("claude", "2026").unwrap().get()).unwrap();
        let reloaded_yearly: Value =
            serde_json::from_str(reloaded.lookup_yearly("claude", "2026").unwrap().get()).unwrap();
        assert_eq!(original_yearly, reloaded_yearly);
    }

    #[tokio::test]
    async fn raw_json_response_sends_original_json_unparsed_and_uncloned() {
        for original in [
            serde_json::json!({ "date": "2026-07-09", "summary": { "total_tokens": 110 } }),
            serde_json::json!({ "month": "2026-07", "total_tokens": 42 }),
            serde_json::json!({ "year": "2026", "total_tokens": 42 }),
        ] {
            let raw = serde_json::value::to_raw_value(&original).unwrap();
            let response = raw_json_response(&raw);
            assert_eq!(response.status(), StatusCode::OK);
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap();
            assert!(
                content_type.starts_with("application/json"),
                "{content_type}"
            );

            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body = String::from_utf8(bytes.to_vec()).unwrap();
            let parsed: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(parsed, original);
        }
    }

    #[tokio::test]
    async fn drive_refresh_reuses_cache_when_version_unchanged_across_two_expiries() {
        let download_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dc = download_count.clone();
        let download = move || {
            let dc = dc.clone();
            async move {
                dc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(test_snapshot("v1"))
            }
        };
        let check_version = || async { Ok::<Option<String>, String>(Some("v1".to_string())) };

        let mut guard: Option<CachedSnapshot> = None;
        let ttl = Duration::ZERO; // 每次呼叫都視為已到期，用來模擬「兩次到期」

        refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(download_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(
            download_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "版本沒變，兩次到期都不應該重新下載"
        );
    }

    #[tokio::test]
    async fn drive_refresh_redownloads_when_version_changes() {
        let download_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dc = download_count.clone();
        let download = move || {
            let dc = dc.clone();
            async move {
                dc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(test_snapshot("content"))
            }
        };
        let version = Arc::new(std::sync::Mutex::new("v1".to_string()));
        let version_for_check = version.clone();
        let check_version = move || {
            let version_for_check = version_for_check.clone();
            async move { Ok::<Option<String>, String>(Some(version_for_check.lock().unwrap().clone())) }
        };

        let mut guard: Option<CachedSnapshot> = None;
        let ttl = Duration::ZERO;

        refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(download_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        *version.lock().unwrap() = "v2".to_string();
        refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(
            download_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "版本變了應該重新下載"
        );
    }

    // F1：查版本失敗時不能直接沿用快取，要繼續嘗試下載；下載成功就要用新內容更新快取
    // （否則查版本這個新請求若在正式環境長期失敗，Cloud Run 會永遠停在第一次載入的舊資料）。
    #[tokio::test]
    async fn drive_refresh_downloads_when_version_check_fails_instead_of_reusing_cache() {
        let download_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dc = download_count.clone();
        let download = move || {
            let dc = dc.clone();
            async move {
                let n = dc.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                Ok(test_snapshot(&format!("content-v{n}")))
            }
        };
        let should_fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let should_fail_for_check = should_fail.clone();
        let check_version = move || {
            let should_fail = should_fail_for_check.clone();
            async move {
                if should_fail.load(std::sync::atomic::Ordering::SeqCst) {
                    Err("模擬查詢版本失敗".to_string())
                } else {
                    Ok(Some("v1".to_string()))
                }
            }
        };

        let mut guard: Option<CachedSnapshot> = None;
        let ttl = Duration::ZERO;

        let first = refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(download_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(first.source, "content-v1");

        should_fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let second = refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(
            download_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "查版本失敗時應該繼續嘗試下載，不能直接沿用快取"
        );
        assert_eq!(
            second.source, "content-v2",
            "下載成功時快取內容應該更新成新版本，不是繼續沿用舊快取"
        );
    }

    // 既有、修改前就有的容錯行為：查版本失敗「且」下載也失敗時，才沿用舊快取。
    #[tokio::test]
    async fn drive_refresh_falls_back_to_cache_only_when_download_also_fails() {
        let download_should_fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let download_should_fail_for_download = download_should_fail.clone();
        let download = move || {
            let should_fail = download_should_fail_for_download.clone();
            async move {
                if should_fail.load(std::sync::atomic::Ordering::SeqCst) {
                    Err("模擬下載失敗".to_string())
                } else {
                    Ok(test_snapshot("content-v1"))
                }
            }
        };
        let check_version =
            || async { Err::<Option<String>, String>("模擬查詢版本失敗".to_string()) };

        let mut guard: Option<CachedSnapshot> = None;
        let ttl = Duration::ZERO;

        // 第一次載入時 guard 是空的，下載成功建立快取
        let first = refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(first.source, "content-v1");

        // 第二次：查版本失敗、下載也失敗 → 沿用舊快取
        download_should_fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let second = refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(
            second.source, "content-v1",
            "查版本與下載都失敗時，應沿用舊快取"
        );
    }

    // R2：Drive 模式下版本比對「相同」而沿用快取時，要重設 loaded_at 讓 TTL 重新倒數，
    // 否則下一次還在 TTL 內的請求也會誤判成到期而再去查一次版本。
    #[tokio::test]
    async fn drive_refresh_resets_loaded_at_when_version_unchanged() {
        let download_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dc = download_count.clone();
        let download = move || {
            let dc = dc.clone();
            async move {
                dc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(test_snapshot("v1"))
            }
        };
        let check_version_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cc = check_version_calls.clone();
        let check_version = move || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok::<Option<String>, String>(Some("v1".to_string()))
            }
        };

        let mut guard: Option<CachedSnapshot> = None;
        let ttl = Duration::from_millis(80);

        // 第一次載入（guard 是空的，一定會查版本＋下載）
        refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(download_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            check_version_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );

        // 等到 TTL 到期，版本查詢回「相同」，沿用快取——若 loaded_at 沒有被重設，
        // 下一步驟就會立刻視為又到期。
        tokio::time::sleep(Duration::from_millis(120)).await;
        refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(
            download_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "版本沒變不應該重新下載"
        );
        assert_eq!(
            check_version_calls.load(std::sync::atomic::Ordering::SeqCst),
            2
        );

        // 緊接著在 TTL 還沒到期的情況下再打一次：不應該再去查版本，
        // 因為上一步驟應該已經把 loaded_at 重設過。
        refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(
            check_version_calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "loaded_at 沒有被重設的話，這裡就會誤判成到期又去查一次版本"
        );
    }

    // R4：本機檔案模式（TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH）完全不查 Drive 版本，
    // 到期就直接重新載入檔案；即使同時設定 DRIVE_SNAPSHOT_FILE_ID 也一樣。
    #[tokio::test]
    async fn file_path_mode_never_queries_drive_version_even_when_drive_id_also_set() {
        // 與 get_cached_snapshot() 相同的路由邏輯：檔案路徑有設定時，優先走本機檔案模式
        assert!(
            !should_use_drive_version_check(true, true),
            "同時設定檔案路徑與 Drive file id 時，應優先走本機檔案模式、不查版本"
        );
        assert!(should_use_drive_version_check(false, true));
        assert!(!should_use_drive_version_check(true, false));
        assert!(!should_use_drive_version_check(false, false));

        let check_version_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cc = check_version_calls.clone();
        let check_version = move || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok::<Option<String>, String>(Some("v1".to_string()))
            }
        };
        let no_version_check = || async { Ok::<Option<String>, String>(None) };
        let download = || async { Ok(test_snapshot("file-content")) };

        let use_drive_version_check = should_use_drive_version_check(true, true);
        let mut guard: Option<CachedSnapshot> = None;
        let ttl = Duration::ZERO;

        for _ in 0..2 {
            if use_drive_version_check {
                refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
                    .await
                    .unwrap();
            } else {
                refresh_cached_snapshot(&mut guard, ttl, &no_version_check, &download)
                    .await
                    .unwrap();
            }
        }

        assert_eq!(
            check_version_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "檔案模式完全不應呼叫查版本函式"
        );
    }

    // R7：Drive 回應沒有 version 也沒有 modifiedTime（查詢函式回傳 None）時，
    // 不能跟「快取也沒記過版本」湊成一對就當成「沒變」，要當成不同、重新下載。
    #[tokio::test]
    async fn drive_refresh_redownloads_when_both_versions_are_none() {
        let download_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dc = download_count.clone();
        let download = move || {
            let dc = dc.clone();
            async move {
                let n = dc.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                Ok(test_snapshot(&format!("content-v{n}")))
            }
        };
        // 模擬 Drive 回應缺 version 與 modifiedTime 欄位：查詢函式回 Ok(None)
        let check_version = || async { Ok::<Option<String>, String>(None) };

        let mut guard: Option<CachedSnapshot> = None;
        let ttl = Duration::ZERO;

        let first = refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(download_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(first.source, "content-v1");

        let second = refresh_cached_snapshot(&mut guard, ttl, &check_version, &download)
            .await
            .unwrap();
        assert_eq!(
            download_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "兩邊版本都是 None 時不能當成沒變，必須重新下載"
        );
        assert_eq!(second.source, "content-v2");
    }

    fn test_snapshot(source: &str) -> DashboardSnapshot {
        DashboardSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            generated_at: "2026-09-28T00:00:00Z".to_string(),
            source: source.to_string(),
            assistants: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn snapshot_export_propagates_fetch_errors() {
        let result = build_snapshot_with(&["claude"], |_, view, _| async move {
            match view {
                SnapshotView::Dates => Err("資料庫暫時無法開啟".to_string()),
                _ => Ok(None),
            }
        })
        .await;

        assert_eq!(result.unwrap_err(), "資料庫暫時無法開啟");
    }

    #[tokio::test]
    async fn snapshot_export_includes_newer_assistants() {
        let snapshot = build_snapshot_with(&ASSISTANTS, |assistant, view, _| async move {
            Ok(match view {
                SnapshotView::Dates if assistant == "muse" => {
                    Some(serde_json::json!({ "dates": ["2026-09-01"] }))
                }
                SnapshotView::Daily => Some(serde_json::json!({ "date": "2026-09-01" })),
                _ => None,
            })
        })
        .await
        .unwrap();

        assert_eq!(
            snapshot
                .lookup_assistant("muse-code")
                .map(|item| item.dates.clone()),
            Some(vec!["2026-09-01".to_string()])
        );
        assert!(snapshot.lookup_daily("muse", "2026-09-01").is_some());
    }

    #[test]
    fn snapshot_assistants_match_handlers_supported_assistants() {
        let source = include_str!("handlers/mod.rs");
        let fn_body = source
            .split("pub fn is_supported_assistant")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("handlers/mod.rs 應定義 is_supported_assistant");
        let mut supported: Vec<&str> = fn_body.split('"').skip(1).step_by(2).collect();
        supported.sort_unstable();
        let mut ours = ASSISTANTS.to_vec();
        ours.sort_unstable();

        assert!(
            !supported.is_empty(),
            "無法從 is_supported_assistant 解析出助理名單"
        );
        assert_eq!(
            ours, supported,
            "snapshot::ASSISTANTS 必須與 handlers::is_supported_assistant 一致，否則新助理在 Cloud Run 看板會消失"
        );
    }

    #[test]
    fn snapshot_setup_info_covers_every_assistant() {
        let info = snapshot_setup_info();
        for assistant in ASSISTANTS {
            assert_eq!(
                info[assistant]["exists"], true,
                "setup-info 缺少 {assistant}"
            );
        }
    }

    #[tokio::test]
    async fn unsupported_endpoints_return_json_not_implemented() {
        let response = unsupported_in_snapshot_mode().await.into_response();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert!(payload["error"].as_str().unwrap().contains("snapshot"));
    }

    fn cli_args(list: &[&str]) -> Vec<String> {
        std::iter::once("token-usage-insights")
            .chain(list.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn export_snapshot_flag_is_only_accepted_as_first_argument() {
        assert_eq!(
            export_snapshot_path(&cli_args(&["--export-snapshot", "out.json"]), None),
            Some(PathBuf::from("out.json"))
        );
        for rejected in [
            vec!["export", "--out", "--export-snapshot", "x.json"],
            vec!["--", "--export-snapshot", "x.json"],
            vec!["import", "--file", "--export-snapshot"],
            vec!["--export-snapshot"],
            vec!["--export-snapshot", ""],
            vec!["--export-snapshot", "   "],
            vec!["--export-snapshot", "--help"],
            vec!["--export-snapshot", "out.json", "update", "--force"],
        ] {
            assert_eq!(
                export_snapshot_path(&cli_args(&rejected), None),
                None,
                "{rejected:?}"
            );
        }
    }

    #[test]
    fn export_snapshot_env_only_applies_without_arguments() {
        let env = Some("env.json".to_string());
        assert_eq!(
            export_snapshot_path(&cli_args(&[]), env.clone()),
            Some(PathBuf::from("env.json"))
        );
        for with_args in [
            vec!["--help"],
            vec!["update", "--check"],
            vec!["export", "--out", "x.json"],
        ] {
            assert_eq!(
                export_snapshot_path(&cli_args(&with_args), env.clone()),
                None,
                "{with_args:?}"
            );
        }
        assert_eq!(
            export_snapshot_path(&cli_args(&[]), Some("  ".to_string())),
            None
        );
    }

    #[tokio::test]
    async fn dashboard_response_404_means_no_data_and_other_failures_propagate() {
        let json_response = |status: StatusCode, body: Value| (status, Json(body)).into_response();

        assert_eq!(
            response_to_optional_json(
                "t",
                json_response(StatusCode::NOT_FOUND, serde_json::json!({ "error": "x" }))
            )
            .await,
            Ok(None)
        );
        assert_eq!(
            response_to_optional_json(
                "t",
                json_response(
                    StatusCode::OK,
                    serde_json::json!({ "dates": ["2026-09-01"] })
                )
            )
            .await,
            Ok(Some(serde_json::json!({ "dates": ["2026-09-01"] })))
        );

        let error = response_to_optional_json(
            "Daily claude/2026-09-01",
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": "database is locked" }),
            ),
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("500") && error.contains("database is locked"),
            "{error}"
        );

        let error =
            response_to_optional_json("t", (StatusCode::BAD_REQUEST, "plain text").into_response())
                .await
                .unwrap_err();
        assert!(error.contains("400"), "{error}");
    }

    #[tokio::test]
    async fn session_details_rejects_unsafe_session_ids_before_loading_snapshot() {
        let too_long = "a".repeat(129);
        for session_id in [
            "..",
            ".",
            "a/b",
            "a\\b",
            "%2e%2e",
            "a b",
            "",
            too_long.as_str(),
        ] {
            let response =
                get_session_details(AxumPath(("claude".to_string(), session_id.to_string())))
                    .await
                    .into_response();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{session_id:?}");
        }
        assert!(is_safe_session_id(&"a".repeat(128)));
        assert!(is_safe_session_id("019a-b_c.1"));
    }

    #[test]
    fn upload_script_assistant_list_matches_snapshot_assistants() {
        let script = include_str!("../scripts/upload-drive-snapshot.ps1");
        let line = script
            .lines()
            .find(|line| line.trim_start().starts_with("$Assistants = @("))
            .expect("upload-drive-snapshot.ps1 應定義 $Assistants");
        let mut names: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
        names.sort_unstable();
        let mut ours = ASSISTANTS.to_vec();
        ours.sort_unstable();

        assert_eq!(
            names, ours,
            "upload-drive-snapshot.ps1 的 $Assistants 必須與 snapshot::ASSISTANTS 一致"
        );
    }

    #[test]
    fn percent_encode_encodes_drive_scope_for_metadata_url() {
        assert_eq!(
            percent_encode("https://www.googleapis.com/auth/drive.readonly"),
            "https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.readonly"
        );
    }

    #[test]
    fn snapshot_deserializes_assistant_lists_with_nulls() {
        let raw = r#"{
            "schema_version": 1,
            "generated_at": "2026-07-09T00:00:00Z",
            "source": "test",
            "assistants": {
                "cursor": {
                    "dates": [null, "2026-07-09", ""],
                    "months": [null, "2026-07"],
                    "years": [null, "2026"],
                    "daily": {},
                    "monthly": {},
                    "yearly": {}
                }
            }
        }"#;

        let snapshot: DashboardSnapshot = serde_json::from_str(raw).unwrap();
        let cursor = snapshot.assistants.get("cursor").unwrap();
        assert_eq!(cursor.dates, vec!["2026-07-09"]);
        assert_eq!(cursor.months, vec!["2026-07"]);
        assert_eq!(cursor.years, vec!["2026"]);
    }

    #[test]
    fn snapshot_deserializes_optional_session_events() {
        let raw = r#"{
            "schema_version": 1,
            "generated_at": "2026-07-09T00:00:00Z",
            "source": "test",
            "assistants": {
                "codex": {
                    "dates": ["2026-07-09"],
                    "months": ["2026-07"],
                    "years": ["2026"],
                    "daily": {},
                    "monthly": {},
                    "yearly": {},
                    "session_events": {
                        "session-1": {
                            "drive_file_id": "drive-file-123",
                            "file_name": "token-usage-insights-session-codex-session-1.json",
                            "content_type": "application/json",
                            "uploaded_at": "2026-07-09T00:00:00Z"
                        }
                    }
                },
                "claude": {
                    "dates": [],
                    "months": [],
                    "years": [],
                    "daily": {},
                    "monthly": {},
                    "yearly": {}
                }
            }
        }"#;

        let snapshot: DashboardSnapshot = serde_json::from_str(raw).unwrap();
        assert_eq!(
            snapshot
                .lookup_session_event("codex", "session-1")
                .unwrap()
                .drive_file_id,
            "drive-file-123"
        );
        assert!(snapshot
            .lookup_session_event("claude", "session-1")
            .is_none());
    }

    #[test]
    fn empty_env_var_is_not_treated_as_configured() {
        std::env::set_var("TOKEN_USAGE_INSIGHTS_EMPTY_TEST", "  ");
        assert!(!env_var_is_set("TOKEN_USAGE_INSIGHTS_EMPTY_TEST"));
        std::env::set_var("TOKEN_USAGE_INSIGHTS_EMPTY_TEST", "snapshot.json");
        assert!(env_var_is_set("TOKEN_USAGE_INSIGHTS_EMPTY_TEST"));
        std::env::remove_var("TOKEN_USAGE_INSIGHTS_EMPTY_TEST");
    }
}
