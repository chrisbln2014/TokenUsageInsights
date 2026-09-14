use axum::{extract::Path as AxumPath, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
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
const ASSISTANTS: [&str; 9] = [
    "antigravity",
    "copilot",
    "codex",
    "claude",
    "cursor",
    "grok",
    "pi",
    "omp",
    "muse",
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
    pub daily: HashMap<String, Value>,
    pub monthly: HashMap<String, Value>,
    pub yearly: HashMap<String, Value>,
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

    pub fn lookup_daily(&self, assistant: &str, date: &str) -> Option<&Value> {
        self.lookup_assistant(assistant)?.daily.get(date)
    }

    pub fn lookup_monthly(&self, assistant: &str, year_month: &str) -> Option<&Value> {
        self.lookup_assistant(assistant)?.monthly.get(year_month)
    }

    pub fn lookup_yearly(&self, assistant: &str, year: &str) -> Option<&Value> {
        self.lookup_assistant(assistant)?.yearly.get(year)
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
        let monthly = fetch_view_map(&fetch, &assistant, SnapshotView::Monthly, &months).await?;
        let yearly = fetch_view_map(&fetch, &assistant, SnapshotView::Yearly, &years).await?;

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

    if let Some(cached) = guard.as_ref() {
        if cached.loaded_at.elapsed() < ttl {
            return Ok(cached.snapshot.clone());
        }
    }

    let snapshot = match load_snapshot_from_env().await {
        Ok(snapshot) => Arc::new(snapshot),
        Err(err) => {
            if let Some(cached) = guard.as_ref() {
                eprintln!("⚠️ 重新載入 snapshot 失敗，使用快取資料: {err}");
                return Ok(cached.snapshot.clone());
            }
            return Err(err);
        }
    };
    *guard = Some(CachedSnapshot {
        loaded_at: Instant::now(),
        snapshot: snapshot.clone(),
    });
    Ok(snapshot)
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
                Json(value.clone()).into_response()
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
                Json(value.clone()).into_response()
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
                Json(value.clone()).into_response()
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
                    "Snapshot 中沒有此 Session 的事件檔，請重新上傳 Google Drive snapshot。",
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

        let daily = snapshot.lookup_daily("antigravity", "2026-07-09").unwrap();
        assert_eq!(daily["summary"]["total_tokens"], 110);
        assert_eq!(daily["raw_entries"][0]["session_id"], "session-1");
        assert!(daily["raw_entries"][0]["transcript_path"].is_null());

        assert!(snapshot.lookup_monthly("antigravity", "2026-07").is_none());
        assert_eq!(
            snapshot.lookup_yearly("antigravity", "2026").unwrap()["year"],
            "2026"
        );
        assert!(snapshot
            .lookup_assistant("claude")
            .unwrap()
            .dates
            .is_empty());
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
