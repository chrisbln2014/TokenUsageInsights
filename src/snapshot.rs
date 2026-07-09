use axum::{extract::Path as AxumPath, http::StatusCode, response::IntoResponse, Json};
use rusqlite::Connection;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

use crate::db::{self, UsageEntry};
use crate::handlers::{
    normalize_assistant_name, AgentBreakdown, DateListResponse, DaySummary,
    MonthlyDailyBreakdown, MonthlyDetailsResponse, MonthlyModelSummary, MonthlyProjectSummary,
    MonthListResponse, SessionSummary, UsageDetailsResponse, YearListResponse,
    YearlyDetailsResponse, YearlyMonthlyBreakdown,
};
use crate::pricing::{calculate_cost, load_pricing_rules};

const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const DEFAULT_REFRESH_SECONDS: u64 = 300;
const ASSISTANTS: [&str; 5] = ["antigravity", "copilot", "codex", "claude", "cursor"];

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
        self.lookup_assistant(assistant)?.session_events.get(session_id)
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
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--export-snapshot" {
            return args.next().map(PathBuf::from);
        }
    }

    std::env::var("TOKEN_USAGE_INSIGHTS_EXPORT_SNAPSHOT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

pub fn build_snapshot_from_conn(
    conn: &Connection,
    assistants: &[&str],
) -> Result<DashboardSnapshot, String> {
    let mut assistant_snapshots = HashMap::new();

    for assistant in assistants {
        let assistant = normalize_assistant_name(assistant);
        let dates = db::get_available_dates(conn, &assistant)?;
        let months = db::get_available_months(conn, &assistant)?;
        let years = db::get_available_years(conn, &assistant)?;

        let mut daily = HashMap::new();
        for date in &dates {
            let entries = db::get_usage_entries_by_date(conn, date, &assistant)?;
            if entries.is_empty() {
                continue;
            }
            let response = build_daily_response(date.clone(), entries)?;
            daily.insert(date.clone(), serde_json::to_value(response).map_err(|e| e.to_string())?);
        }

        let mut monthly = HashMap::new();
        for month in &months {
            let entries = db::get_usage_entries_by_month(conn, month, &assistant)?;
            if entries.is_empty() {
                continue;
            }
            let response = build_monthly_response(month.clone(), entries)?;
            monthly.insert(
                month.clone(),
                serde_json::to_value(response).map_err(|e| e.to_string())?,
            );
        }

        let mut yearly = HashMap::new();
        for year in &years {
            let entries = db::get_usage_entries_by_year(conn, year, &assistant)?;
            if entries.is_empty() {
                continue;
            }
            let response = build_yearly_response(year.clone(), entries)?;
            yearly.insert(year.clone(), serde_json::to_value(response).map_err(|e| e.to_string())?);
        }

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

pub fn write_snapshot_file(conn: &Connection, path: &Path) -> Result<(), String> {
    let snapshot = build_snapshot_from_conn(conn, &ASSISTANTS)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("建立 snapshot 目錄失敗: {e}"))?;
        }
    }
    let data = serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?;
    fs::write(path, data).map_err(|e| format!("寫入 snapshot 失敗: {e}"))
}

fn sanitize_raw_entry(mut entry: UsageEntry) -> UsageEntry {
    entry.transcript_path = None;
    entry
}

fn build_daily_response(
    date: String,
    entries_with_type: Vec<(UsageEntry, String)>,
) -> Result<UsageDetailsResponse, String> {
    if entries_with_type.is_empty() {
        return Err("找不到該日期的使用量資料。".to_string());
    }

    let mut summary = DaySummary::default();
    let mut sessions_map: HashMap<String, (Vec<UsageEntry>, String)> = HashMap::new();
    let mut raw_entries = Vec::new();

    for (entry, assistant_type) in &entries_with_type {
        raw_entries.push(sanitize_raw_entry(entry.clone()));
        let (list, _) = sessions_map
            .entry(entry.session_id.clone())
            .or_insert_with(|| (Vec::new(), assistant_type.clone()));
        list.push(entry.clone());
    }

    summary.total_sessions = sessions_map.len();
    let pricing_rules = load_pricing_rules();
    let mut sessions_summary = Vec::new();

    for (session_id, (session_entries, assistant_type)) in &sessions_map {
        let last_entry = latest_entry(session_entries);
        let session_totals = sum_session_tokens(session_entries, &last_entry);

        summary.total_tokens += session_totals.total;
        summary.total_input_tokens += session_totals.input;
        summary.total_output_tokens += session_totals.output;
        summary.total_cache_read_tokens += session_totals.cache_read;
        summary.total_cache_write_tokens += session_totals.cache_write;
        summary.total_reasoning_tokens += session_totals.reasoning;

        let session_duration = last_entry
            .cost
            .as_ref()
            .and_then(|c| c.total_api_duration_ms)
            .unwrap_or(0.0) as u64;
        let session_requests = last_entry
            .cost
            .as_ref()
            .and_then(|c| c.total_premium_requests)
            .unwrap_or(0.0) as u64;
        summary.total_duration_ms += session_duration;
        summary.total_requests += session_requests;

        let model = last_entry
            .model
            .clone()
            .unwrap_or_else(|| "Unknown Model".to_string());
        let cost_usd = calculate_cost(
            &pricing_rules,
            &model,
            session_totals.input,
            session_totals.output,
            session_totals.cache_read,
        )
        .unwrap_or(0.0);
        summary.total_cost_usd += cost_usd;

        sessions_summary.push(SessionSummary {
            session_id: session_id.clone(),
            session_name: last_entry
                .session_name
                .clone()
                .unwrap_or_else(|| "Start Coding Session".to_string()),
            assistant_type: assistant_type.clone(),
            cwd: last_entry.cwd.clone().unwrap_or_default(),
            model,
            total_tokens: session_totals.total,
            total_input_tokens: session_totals.input,
            total_output_tokens: session_totals.output,
            total_cache_read_tokens: session_totals.cache_read,
            total_cache_write_tokens: session_totals.cache_write,
            total_reasoning_tokens: session_totals.reasoning,
            max_turn_no: session_entries.iter().map(|e| e.turn_no).max().unwrap_or(1),
            timestamp: session_entries
                .first()
                .map(|e| e.timestamp.clone())
                .unwrap_or_default(),
            duration_ms: session_duration,
            cost_usd,
            parent_session_id: last_entry.parent_session_id.clone(),
            agent_nickname: last_entry.agent_nickname.clone(),
            agent_role: last_entry.agent_role.clone(),
            reasoning_effort: last_entry.reasoning_effort.clone(),
        });
    }

    sessions_summary.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    Ok(UsageDetailsResponse {
        date,
        summary,
        sessions: sessions_summary,
        raw_entries,
    })
}

#[derive(Default)]
struct SessionTokenTotals {
    total: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
}

fn latest_entry(entries: &[UsageEntry]) -> UsageEntry {
    entries
        .iter()
        .max_by_key(|entry| entry.turn_no)
        .cloned()
        .unwrap_or_else(|| entries[0].clone())
}

fn sum_session_tokens(entries: &[UsageEntry], last_entry: &UsageEntry) -> SessionTokenTotals {
    let mut totals = SessionTokenTotals::default();

    for entry in entries {
        if let Some(tokens) = &entry.delta_tokens {
            totals.total += tokens.total;
            totals.input += tokens.input;
            totals.output += tokens.output;
            totals.cache_read += tokens.cache_read.unwrap_or(0);
            totals.cache_write += tokens.cache_write.unwrap_or(0);
            totals.reasoning += tokens.reasoning.unwrap_or(0);
        }
    }

    if totals.total > 0 {
        return totals;
    }

    if let Some(tokens) = &last_entry.tokens {
        totals.total = tokens.total;
        totals.input = tokens.input;
        totals.output = tokens.output;
        totals.cache_read = tokens.cache_read.unwrap_or(0);
        totals.cache_write = tokens.cache_write.unwrap_or(0);
        totals.reasoning = tokens.reasoning.unwrap_or(0);
    }

    totals
}

fn build_monthly_response(
    year_month: String,
    entries_with_type: Vec<(UsageEntry, String, String)>,
) -> Result<MonthlyDetailsResponse, String> {
    if entries_with_type.is_empty() {
        return Err("找不到該月份的使用量資料。".to_string());
    }

    let mut daily_map: HashMap<String, Vec<(UsageEntry, String)>> = HashMap::new();
    let mut sessions_map: HashMap<String, (Vec<UsageEntry>, String)> = HashMap::new();

    for (entry, assistant_type, date) in entries_with_type {
        daily_map
            .entry(date)
            .or_default()
            .push((entry.clone(), assistant_type.clone()));
        sessions_map
            .entry(entry.session_id.clone())
            .or_insert_with(|| (Vec::new(), assistant_type))
            .0
            .push(entry);
    }

    let mut summary = DaySummary {
        total_sessions: sessions_map.len(),
        ..Default::default()
    };
    let mut daily_breakdown = Vec::new();
    let mut sorted_dates: Vec<String> = daily_map.keys().cloned().collect();
    sorted_dates.sort();

    for date in sorted_dates {
        let day_response = build_daily_response(
            date.clone(),
            daily_map.get(&date).cloned().unwrap_or_default(),
        )?;
        summary.total_tokens += day_response.summary.total_tokens;
        summary.total_input_tokens += day_response.summary.total_input_tokens;
        summary.total_output_tokens += day_response.summary.total_output_tokens;
        summary.total_cache_read_tokens += day_response.summary.total_cache_read_tokens;
        summary.total_cache_write_tokens += day_response.summary.total_cache_write_tokens;
        summary.total_reasoning_tokens += day_response.summary.total_reasoning_tokens;
        summary.total_duration_ms += day_response.summary.total_duration_ms;
        summary.total_requests += day_response.summary.total_requests;
        summary.total_cost_usd += day_response.summary.total_cost_usd;

        daily_breakdown.push(MonthlyDailyBreakdown {
            date,
            total_tokens: day_response.summary.total_tokens,
            total_input_tokens: day_response.summary.total_input_tokens,
            total_output_tokens: day_response.summary.total_output_tokens,
            total_cache_read_tokens: day_response.summary.total_cache_read_tokens,
            total_reasoning_tokens: day_response.summary.total_reasoning_tokens,
            sessions_count: day_response.summary.total_sessions,
            cost_usd: day_response.summary.total_cost_usd,
        });
    }

    let (projects, models, agent_breakdown) = summarize_sessions(&sessions_map);

    Ok(MonthlyDetailsResponse {
        year_month,
        summary,
        daily_breakdown,
        projects,
        models,
        agent_breakdown,
    })
}

fn build_yearly_response(
    year: String,
    entries_with_type: Vec<(UsageEntry, String, String)>,
) -> Result<YearlyDetailsResponse, String> {
    if entries_with_type.is_empty() {
        return Err("找不到該年份的使用量資料。".to_string());
    }

    let mut monthly_map: HashMap<String, Vec<(UsageEntry, String, String)>> = HashMap::new();
    let mut sessions_map: HashMap<String, (Vec<UsageEntry>, String)> = HashMap::new();

    for (entry, assistant_type, date) in entries_with_type {
        let month = date.get(0..7).unwrap_or("Unknown").to_string();
        monthly_map
            .entry(month)
            .or_default()
            .push((entry.clone(), assistant_type.clone(), date));
        sessions_map
            .entry(entry.session_id.clone())
            .or_insert_with(|| (Vec::new(), assistant_type))
            .0
            .push(entry);
    }

    let mut summary = DaySummary {
        total_sessions: sessions_map.len(),
        ..Default::default()
    };
    let mut monthly_breakdown = Vec::new();
    let mut sorted_months: Vec<String> = monthly_map.keys().cloned().collect();
    sorted_months.sort();

    for month in sorted_months {
        let month_response = build_monthly_response(
            month.clone(),
            monthly_map.get(&month).cloned().unwrap_or_default(),
        )?;
        summary.total_tokens += month_response.summary.total_tokens;
        summary.total_input_tokens += month_response.summary.total_input_tokens;
        summary.total_output_tokens += month_response.summary.total_output_tokens;
        summary.total_cache_read_tokens += month_response.summary.total_cache_read_tokens;
        summary.total_cache_write_tokens += month_response.summary.total_cache_write_tokens;
        summary.total_reasoning_tokens += month_response.summary.total_reasoning_tokens;
        summary.total_duration_ms += month_response.summary.total_duration_ms;
        summary.total_requests += month_response.summary.total_requests;
        summary.total_cost_usd += month_response.summary.total_cost_usd;

        monthly_breakdown.push(YearlyMonthlyBreakdown {
            month,
            total_tokens: month_response.summary.total_tokens,
            total_input_tokens: month_response.summary.total_input_tokens,
            total_output_tokens: month_response.summary.total_output_tokens,
            total_cache_read_tokens: month_response.summary.total_cache_read_tokens,
            total_reasoning_tokens: month_response.summary.total_reasoning_tokens,
            sessions_count: month_response.summary.total_sessions,
            cost_usd: month_response.summary.total_cost_usd,
        });
    }

    let (projects, models, agent_breakdown) = summarize_sessions(&sessions_map);

    Ok(YearlyDetailsResponse {
        year,
        summary,
        monthly_breakdown,
        projects,
        models,
        agent_breakdown,
    })
}

fn summarize_sessions(
    sessions_map: &HashMap<String, (Vec<UsageEntry>, String)>,
) -> (
    Vec<MonthlyProjectSummary>,
    Vec<MonthlyModelSummary>,
    HashMap<String, AgentBreakdown>,
) {
    let pricing_rules = load_pricing_rules();
    let mut project_map: HashMap<String, (usize, u64, f64)> = HashMap::new();
    let mut model_map: HashMap<String, (usize, u64, u64, u64, u64, f64)> = HashMap::new();
    let mut agent_breakdown: HashMap<String, AgentBreakdown> = HashMap::new();

    for (entries, assistant_type) in sessions_map.values() {
        if entries.is_empty() {
            continue;
        }
        let last_entry = latest_entry(entries);
        let totals = sum_session_tokens(entries, &last_entry);
        let model = last_entry
            .model
            .clone()
            .unwrap_or_else(|| "Unknown Model".to_string());
        let cost_usd = calculate_cost(
            &pricing_rules,
            &model,
            totals.input,
            totals.output,
            totals.cache_read,
        )
        .unwrap_or(0.0);

        let cwd = last_entry
            .cwd
            .clone()
            .unwrap_or_else(|| "Unknown CWD".to_string());
        let project_stat = project_map.entry(cwd).or_insert((0, 0, 0.0));
        project_stat.0 += 1;
        project_stat.1 += totals.total;
        project_stat.2 += cost_usd;

        let model_stat = model_map.entry(model).or_insert((0, 0, 0, 0, 0, 0.0));
        model_stat.0 += 1;
        model_stat.1 += totals.total;
        model_stat.2 += totals.input;
        model_stat.3 += totals.output;
        model_stat.4 += totals.cache_read;
        model_stat.5 += cost_usd;

        let agent_stat = agent_breakdown.entry(assistant_type.clone()).or_default();
        agent_stat.total_tokens += totals.total;
        agent_stat.total_input_tokens += totals.input;
        agent_stat.total_output_tokens += totals.output;
        agent_stat.total_cache_read_tokens += totals.cache_read;
        agent_stat.total_reasoning_tokens += totals.reasoning;
        agent_stat.total_cost_usd += cost_usd;
        agent_stat.total_sessions += 1;
    }

    let mut projects = project_map
        .into_iter()
        .map(|(cwd, (sessions_count, total_tokens, cost_usd))| MonthlyProjectSummary {
            cwd,
            sessions_count,
            total_tokens,
            cost_usd,
        })
        .collect::<Vec<_>>();
    projects.sort_by_key(|item| std::cmp::Reverse(item.total_tokens));

    let mut models = model_map
        .into_iter()
        .map(
            |(
                model,
                (
                    sessions_count,
                    total_tokens,
                    total_input_tokens,
                    total_output_tokens,
                    total_cache_read_tokens,
                    cost_usd,
                ),
            )| MonthlyModelSummary {
                model,
                sessions_count,
                total_tokens,
                total_input_tokens,
                total_output_tokens,
                total_cache_read_tokens,
                cost_usd,
            },
        )
        .collect::<Vec<_>>();
    models.sort_by_key(|item| std::cmp::Reverse(item.total_tokens));

    (projects, models, agent_breakdown)
}

async fn load_snapshot_from_env() -> Result<DashboardSnapshot, String> {
    if let Ok(path) = std::env::var("TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH") {
        let data = fs::read_to_string(&path)
            .map_err(|e| format!("讀取 snapshot 檔案失敗 ({path}): {e}"))?;
        return serde_json::from_str(&data).map_err(|e| format!("解析 snapshot JSON 失敗: {e}"));
    }

    if let Ok(file_id) = std::env::var("DRIVE_SNAPSHOT_FILE_ID") {
        let data = fetch_drive_file(&file_id).await?;
        return serde_json::from_str(&data).map_err(|e| format!("解析 Drive snapshot JSON 失敗: {e}"));
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
        format!(
            "透過 IAM Credentials 取得 Drive scoped token 失敗 ({service_account_email}): {e}"
        )
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

    Json(serde_json::json!({
        "workspace_dir": "Cloud Run snapshot mode",
        "home_dir": "Google Drive snapshot",
        "antigravity": { "dir_path": "snapshot", "exists": true, "script_path": "" },
        "copilot": { "dir_path": "snapshot", "exists": true, "script_path": "" },
        "codex": { "dir_path": "snapshot", "exists": true, "script_path": "" },
        "claude": { "dir_path": "snapshot", "exists": true, "script_path": "" },
        "cursor": { "dir_path": "snapshot", "exists": true, "script_path": "" }
    }))
    .into_response()
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

    fn insert_usage_entry(
        conn: &Connection,
        assistant: &str,
        date: &str,
        session_id: &str,
        turn_no: i64,
        delta_total: i64,
    ) {
        conn.execute(
            "INSERT INTO usage_entries (
                assistant_type, timestamp, date, session_id, session_name, cwd, turn_no, model,
                tokens_input, tokens_output, tokens_cache_read, tokens_cache_write, tokens_reasoning, tokens_total,
                delta_input, delta_output, delta_cache_read, delta_cache_write, delta_reasoning, delta_total
            ) VALUES (
                ?, ?, ?, ?, 'Snapshot test', '/workspace/token-dashboard', ?, 'Gemini 3.5 Flash',
                100, 30, 20, 5, 7, 150,
                80, 20, 10, 4, 6, ?
            )",
            rusqlite::params![
                assistant,
                format!("{date} 10:0{turn_no}:00"),
                date,
                session_id,
                turn_no,
                delta_total,
            ],
        )
        .unwrap();
    }

    #[test]
    fn snapshot_export_precomputes_dashboard_api_responses() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_db(&conn).unwrap();
        insert_usage_entry(&conn, "antigravity", "2026-07-09", "session-1", 1, 110);
        insert_usage_entry(&conn, "claude", "2026-07-09", "session-2", 1, 220);

        let snapshot = build_snapshot_from_conn(&conn, &["antigravity", "claude"]).unwrap();

        assert_eq!(snapshot.schema_version, 1);
        assert_eq!(
            snapshot.assistants.get("antigravity").unwrap().dates,
            vec!["2026-07-09"]
        );

        let daily = snapshot.lookup_daily("antigravity", "2026-07-09").unwrap();
        assert_eq!(daily["date"], "2026-07-09");
        assert_eq!(daily["summary"]["total_tokens"], 110);
        assert_eq!(daily["sessions"][0]["session_id"], "session-1");
        assert!(daily["raw_entries"][0]["transcript_path"].is_null());

        let monthly = snapshot.lookup_monthly("claude", "2026-07").unwrap();
        assert_eq!(monthly["year_month"], "2026-07");
        assert_eq!(monthly["summary"]["total_tokens"], 220);

        let yearly = snapshot.lookup_yearly("claude", "2026").unwrap();
        assert_eq!(yearly["year"], "2026");
        assert_eq!(yearly["summary"]["total_tokens"], 220);
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
        assert!(snapshot.lookup_session_event("claude", "session-1").is_none());
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
