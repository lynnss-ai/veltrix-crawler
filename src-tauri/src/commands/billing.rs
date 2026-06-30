//! 账单计费:模型 token 用量与请求次数统计。

use chrono::{Local, NaiveDate};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use serde::Serialize;
use tauri::State;
use veltrix_core::error::{CrawlerError, Result};

use super::{current_user, AppState};

/// 默认统计天数。
const DEFAULT_BILLING_DAYS: i64 = 15;

/// 给定本地自然日,返回其 0 点的 Unix 时间戳(秒)。
fn local_date_start_ts(date: NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0)
        .and_then(|naive| naive.and_local_timezone(Local).single())
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

/// 本地时区「今天 0 点」的 Unix 时间戳(秒)。
fn local_today_start_ts() -> i64 {
    local_date_start_ts(Local::now().date_naive())
}

// ===================== 响应类型 =====================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingOverview {
    pub total_tokens: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_requests: i64,
    pub by_model: Vec<ModelUsage>,
    pub request_by_model: Vec<ModelRequestCount>,
    pub token_trend_dates: Vec<String>,
    pub token_trend_series: Vec<ModelTrendSeries>,
    pub request_trend_dates: Vec<String>,
    pub request_trend_series: Vec<ModelTrendSeries>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub last_requested_at: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequestCount {
    pub model: String,
    pub count: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelTrendSeries {
    pub model: String,
    pub values: Vec<i64>,
}

// ===================== 聚合查询 =====================

/// 模型用量汇总(GROUP BY model)。
async fn usage_by_model(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
    start: i64,
    end: i64,
) -> Result<Vec<ModelUsage>> {
    let backend = db.get_database_backend();
    let owner_filter = if self_only {
        format!("AND owner = '{}'", owner.replace('\'', "''"))
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT model, SUM(prompt_tokens) AS pt, SUM(completion_tokens) AS ct, \
         SUM(total_tokens) AS tt, MAX(created_at) AS last_req \
         FROM model_usage_records \
         WHERE created_at >= {start} AND created_at <= {end} {owner_filter} \
         GROUP BY model ORDER BY tt DESC"
    );
    let rows = db
        .query_all(Statement::from_string(backend, sql))
        .await
        .map_err(|e| CrawlerError::Config(format!("查询模型用量失败: {e}")))?;
    Ok(rows
        .iter()
        .map(|r| ModelUsage {
            model: r.try_get("", "model").unwrap_or_default(),
            prompt_tokens: r.try_get("", "pt").unwrap_or(0),
            completion_tokens: r.try_get("", "ct").unwrap_or(0),
            total_tokens: r.try_get("", "tt").unwrap_or(0),
            last_requested_at: r.try_get("", "last_req").unwrap_or(0),
        })
        .collect())
}

/// 模型请求次数(GROUP BY model)。
async fn requests_by_model(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
    start: i64,
    end: i64,
) -> Result<Vec<ModelRequestCount>> {
    let backend = db.get_database_backend();
    let owner_filter = if self_only {
        format!("AND owner = '{}'", owner.replace('\'', "''"))
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT model, COUNT(*) AS cnt \
         FROM model_usage_records \
         WHERE created_at >= {start} AND created_at <= {end} {owner_filter} \
         GROUP BY model ORDER BY cnt DESC"
    );
    let rows = db
        .query_all(Statement::from_string(backend, sql))
        .await
        .map_err(|e| CrawlerError::Config(format!("查询请求次数失败: {e}")))?;
    Ok(rows
        .iter()
        .map(|r| ModelRequestCount {
            model: r.try_get("", "model").unwrap_or_default(),
            count: r.try_get("", "cnt").unwrap_or(0),
        })
        .collect())
}

/// 按天 + 模型聚合趋势数据。先生成完整日期轴,再填入查询结果(无数据日补 0)。
async fn trend_data(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
    start: i64,
    end: i64,
    metric: &str,
) -> Result<(Vec<String>, Vec<ModelTrendSeries>)> {
    let backend = db.get_database_backend();
    let owner_filter = if self_only {
        format!("AND owner = '{}'", owner.replace('\'', "''"))
    } else {
        String::new()
    };

    // 先生成完整日期轴(MM-DD),与 Dashboard 同口径
    let start_date = chrono::DateTime::from_timestamp(start, 0)
        .map(|dt| dt.with_timezone(&Local).date_naive())
        .unwrap_or_else(|| Local::now().date_naive());
    let end_date = chrono::DateTime::from_timestamp(end, 0)
        .map(|dt| dt.with_timezone(&Local).date_naive())
        .unwrap_or_else(|| Local::now().date_naive());
    let mut dates: Vec<String> = Vec::new();
    let mut date_idx: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut d = start_date;
    while d <= end_date && dates.len() < 90 {
        let s = d.format("%Y-%m-%d").to_string();
        date_idx.insert(s.clone(), dates.len());
        dates.push(s);
        d += chrono::Duration::days(1);
    }
    let dlen = dates.len();

    let value_expr = match metric {
        "tokens" => "SUM(total_tokens)",
        "requests" => "COUNT(*)",
        _ => "SUM(total_tokens)",
    };
    let date_expr = match backend {
        DatabaseBackend::Sqlite => {
            "strftime('%Y-%m-%d', created_at, 'unixepoch', 'localtime')"
        }
        _ => "to_char(to_timestamp(created_at), 'YYYY-MM-DD')",
    };
    let sql = format!(
        "SELECT {date_expr} AS day, model, {value_expr} AS val \
         FROM model_usage_records \
         WHERE created_at >= {start} AND created_at <= {end} {owner_filter} \
         GROUP BY day, model ORDER BY day"
    );
    let rows = db
        .query_all(Statement::from_string(backend, sql))
        .await
        .map_err(|e| CrawlerError::Config(format!("查询趋势数据失败: {e}")))?;

    // 模型→每天值序列(先补 0),再按查询结果填入
    let mut model_data: std::collections::HashMap<String, Vec<i64>> =
        std::collections::HashMap::new();
    for row in rows.iter() {
        let day: String = row.try_get("", "day").unwrap_or_default();
        let model: String = row.try_get("", "model").unwrap_or_default();
        let val: i64 = row.try_get("", "val").unwrap_or(0);
        if let Some(&idx) = date_idx.get(&day) {
            let entry = model_data.entry(model).or_insert_with(|| vec![0i64; dlen]);
            entry[idx] = val;
        }
    }

    let series = model_data
        .into_iter()
        .map(|(model, values)| ModelTrendSeries { model, values })
        .collect();

    Ok((dates, series))
}

// ===================== Tauri 命令 =====================

#[tauri::command]
pub async fn billing_overview(
    state: State<'_, AppState>,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<BillingOverview> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let self_only = me.scope == "self";
    let owner = me.name.clone();
    let db = &state.db;

    // 默认区间:最近 15 天
    let today_start = local_today_start_ts();
    let end_ts = end.unwrap_or(today_start + 86400);
    let default_start = today_start - DEFAULT_BILLING_DAYS * 86400;
    let start_ts = start.unwrap_or(default_start);

    let (by_model, request_by_model, token_trend, request_trend) = tokio::try_join!(
        usage_by_model(db, self_only, &owner, start_ts, end_ts),
        requests_by_model(db, self_only, &owner, start_ts, end_ts),
        trend_data(db, self_only, &owner, start_ts, end_ts, "tokens"),
        trend_data(db, self_only, &owner, start_ts, end_ts, "requests"),
    )?;

    let total_tokens: i64 = by_model.iter().map(|m| m.total_tokens).sum();
    let total_prompt_tokens: i64 = by_model.iter().map(|m| m.prompt_tokens).sum();
    let total_completion_tokens: i64 = by_model.iter().map(|m| m.completion_tokens).sum();
    let total_requests: i64 = request_by_model.iter().map(|r| r.count).sum();

    Ok(BillingOverview {
        total_tokens,
        total_prompt_tokens,
        total_completion_tokens,
        total_requests,
        by_model,
        request_by_model,
        token_trend_dates: token_trend.0,
        token_trend_series: token_trend.1,
        request_trend_dates: request_trend.0,
        request_trend_series: request_trend.1,
    })
}
