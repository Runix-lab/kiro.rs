//! Admin API HTTP 处理器

use std::collections::HashMap;

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use futures::StreamExt;
use std::sync::Arc;

use super::{
    client_keys::mask_client_key,
    middleware::AdminState,
    trace_db::TraceQuery,
    types::{
        AddCredentialRequest, AddProxyRequest, AssignProxyRequest, AssignRoundRobinRequest,
        BatchAddProxyRequest, BatchImportEvent, BatchImportRequest, BatchImportSummary,
        ClientKeyItem, ClientKeysResponse, CompleteSocialLoginRequest, CreateClientKeyRequest,
        CreateClientKeyResponse, GlobalProxyResponse, ModelTestRequest,
        SetAccountRpmLimitConfigRequest, SetAccountThrottleConfigRequest, SetDisabledRequest,
        SetGlobalProxyRequest,
        SetLoadBalancingModeRequest, SetLogGovernanceConfigRequest, SetPriorityRequest,
        SetSelfHealConfigRequest,
        SetUpdateConfigRequest, StartIdcLoginRequest, StartSocialLoginRequest, SuccessResponse,
        UpdateAdminKeyRequest, UpdateClientKeyRequest, UpdateCredentialRequest,
        UpdateRefreshTokenRequest,
    },
    usage_stats::{Range, StatsGranularity, StatsQueryWindow},
};

// Path 元组提取：(credential_id, session_id)
type CredSessionPath = (u64, String);

/// GET /api/admin/credentials
/// 获取所有凭据状态
pub async fn get_all_credentials(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_all_credentials();
    Json(response)
}

/// GET /api/admin/credentials/export
/// 导出凭据为兼容 JSON（含 refreshToken 等敏感字段）
///
/// 可选 query 参数 `ids`（逗号分隔）限定导出哪些凭据；省略则导出全部。
pub async fn export_credentials(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let id_filter: Option<std::collections::HashSet<u64>> = params
        .get("ids")
        .map(|raw| {
            raw.split(',')
                .filter_map(|s| {
                    let t = s.trim();
                    if t.is_empty() {
                        None
                    } else {
                        t.parse::<u64>().ok()
                    }
                })
                .collect::<std::collections::HashSet<u64>>()
        })
        .filter(|s| !s.is_empty());

    let response = state.service.export_credentials(id_filter.as_ref());
    Json(response)
}

/// POST /api/admin/credentials/:id/disabled
/// 设置凭据禁用状态
pub async fn set_credential_disabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetDisabledRequest>,
) -> impl IntoResponse {
    match state.service.set_disabled(id, payload.disabled) {
        Ok(_) => {
            let action = if payload.disabled { "禁用" } else { "启用" };
            Json(SuccessResponse::new(format!("凭据 #{} 已{}", id, action))).into_response()
        }
        Err(e) => e.into_http_response(),
    }
}

/// POST /api/admin/credentials/:id/priority
/// 设置凭据优先级
pub async fn set_credential_priority(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetPriorityRequest>,
) -> impl IntoResponse {
    match state.service.set_priority(id, payload.priority) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 优先级已设置为 {}",
            id, payload.priority
        )))
        .into_response(),
        Err(e) => e.into_http_response(),
    }
}

/// POST /api/admin/credentials/:id/reset
/// 重置失败计数并重新启用
pub async fn reset_failure_count(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.reset_and_enable(id) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 失败计数已重置并重新启用",
            id
        )))
        .into_response(),
        Err(e) => e.into_http_response(),
    }
}

/// POST /api/admin/credentials/:id/clear-throttle
/// 手动解除凭据的账号级风控冷却
pub async fn clear_throttle(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.clear_throttle(id) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 风控冷却已解除", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/:id/balance
/// 获取指定凭据的余额
pub async fn get_credential_balance(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.get_balance(id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => e.into_http_response(),
    }
}

/// GET /api/admin/credentials/:id/models
/// 获取指定凭据当前可用的模型列表（按需实时查询上游）
pub async fn get_credential_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.get_available_models(id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => e.into_http_response(),
    }
}

/// GET /api/admin/models
/// 使用账号池当前选中的可用凭据实时查询上游模型列表。
pub async fn get_current_models(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.get_current_available_models().await {
        Ok(response) => Json(response).into_response(),
        Err(error) => error.into_http_response(),
    }
}

/// POST /api/admin/models/test
/// 使用所选模型发送真实的最小化 Kiro 请求。
pub async fn test_model(
    State(state): State<AdminState>,
    Json(request): Json<ModelTestRequest>,
) -> impl IntoResponse {
    match state.service.test_model(request).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => error.into_http_response(),
    }
}

/// POST /api/admin/credentials/disable-quota-exceeded
/// 一键禁用所有"已超额"凭据（remaining ≤ 0 或 usage_percentage ≥ 100）
pub async fn disable_quota_exceeded(State(state): State<AdminState>) -> impl IntoResponse {
    let result = state.service.disable_quota_exceeded();
    Json(result).into_response()
}

/// POST /api/admin/credentials/:id/overage
/// 开启或关闭指定凭据的超额能力
pub async fn set_credential_overage(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<super::types::SetOverageRequest>,
) -> impl IntoResponse {
    match state.service.set_overage(id, payload.enabled).await {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 已{}超额",
            id,
            if payload.enabled { "开启" } else { "关闭" }
        )))
        .into_response(),
        Err(e) => e.into_http_response(),
    }
}

/// POST /api/admin/credentials/overage/enable-all
/// 一键开启所有"可开启超额且当前未开启"凭据的超额（基于 balance_cache 判断）
pub async fn enable_overage_all(State(state): State<AdminState>) -> impl IntoResponse {
    let result = state.service.enable_overage_for_all_capable().await;
    Json(result).into_response()
}

/// POST /api/admin/credentials
/// 添加新凭据
pub async fn add_credential(
    State(state): State<AdminState>,
    Json(payload): Json<AddCredentialRequest>,
) -> impl IntoResponse {
    match state.service.add_credential(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => e.into_http_response(),
    }
}

/// POST /api/admin/credentials/batch-import
///
/// 批量导入凭据。服务端按 `concurrency`（缺省 8，夹取到 [1,16]）有界并发地逐条处理，
/// 结果通过 SSE 流逐条推送（`index` 对应请求数组下标，乱序），末尾一条汇总事件后关闭流。
///
/// `verify = true`（缺省）：add 后取余额验活，失败回滚；`verify = false`：仅 add 落库。
/// 客户端断开（前端 abort / 关闭连接）时，事件写回失败 → 立即停止处理剩余凭据
/// （已在处理中的至多 concurrency 条会自然结束），从而支持"停止导入"。
pub async fn batch_import_credentials(
    State(state): State<AdminState>,
    Json(req): Json<BatchImportRequest>,
) -> Response {
    let concurrency = req.concurrency.unwrap_or(8).clamp(1, 16) as usize;
    let total = req.credentials.len();
    let verify = req.verify;

    let (tx, rx) = futures::channel::mpsc::unbounded::<BatchImportEvent>();
    let service = state.service.clone();

    // 单个 orchestrator 任务：buffer_unordered 提供有界并发，逐条把结果写回 SSE 流。
    tokio::spawn(async move {
        let mut work = futures::stream::iter(req.credentials.into_iter().enumerate())
            .map(|(index, cred_req)| {
                let service = Arc::clone(&service);
                async move {
                    let result = service.import_one_credential(cred_req, verify).await;
                    (index, result)
                }
            })
            .buffer_unordered(concurrency);

        let mut imported = 0_usize;
        let mut verified = 0_usize;
        let mut duplicate = 0_usize;
        let mut failed = 0_usize;
        let mut rolled_back = 0_usize;
        let mut cancelled = false;

        while let Some((index, result)) = work.next().await {
            let event = result.into_event(index);
            match event.status.as_str() {
                "imported" => imported += 1,
                "verified" => verified += 1,
                "duplicate" => duplicate += 1,
                "failed" => {
                    failed += 1;
                    if event.rolled_back == Some(true) {
                        rolled_back += 1;
                    }
                }
                _ => {}
            }
            // 客户端断开（abort / 关闭连接）→ 接收端随响应体被 drop，send 失败：
            // 停止处理剩余凭据。break 会丢弃 buffer_unordered 内 in-flight 的 future。
            if tx.unbounded_send(event).is_err() {
                let processed = imported + verified + duplicate + failed;
                tracing::info!(
                    "批量导入被客户端中断，停止剩余凭据（已完成 {}/{}）",
                    processed,
                    total
                );
                cancelled = true;
                break;
            }
        }

        // 仅在正常结束时发汇总；客户端中断则不发（流已被对端关闭）。
        if !cancelled {
            let summary = BatchImportEvent {
                index: None,
                status: "summary".to_string(),
                credential_id: None,
                email: None,
                usage: None,
                subscription: None,
                error: None,
                rolled_back: None,
                summary: Some(BatchImportSummary {
                    total,
                    imported,
                    verified,
                    duplicate,
                    failed,
                    rolled_back,
                }),
            };
            let _ = tx.unbounded_send(summary);
        }
        // tx 在此 drop，SSE 流随之关闭
    });

    let body = rx.map(|event| {
        let json = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
        Ok::<_, std::io::Error>(Bytes::from(format!("data: {}\n\n", json)))
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(body))
        .unwrap()
}

/// DELETE /api/admin/credentials/:id
/// 删除凭据
pub async fn delete_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_credential(id) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 已删除", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// PUT /api/admin/credentials/:id
/// 更新凭据可编辑字段（email、proxy 等）
pub async fn update_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<UpdateCredentialRequest>,
) -> impl IntoResponse {
    match state.service.update_credential(id, payload) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// PUT /api/admin/credentials/:id/refresh-token
/// 更新已禁用凭据的 refreshToken
pub async fn update_refresh_token(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<UpdateRefreshTokenRequest>,
) -> impl IntoResponse {
    match state.service.update_refresh_token(id, payload) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} refreshToken 已更新（当前仍为禁用状态，请手动启用）",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/refresh
/// 强制刷新凭据 Token
pub async fn force_refresh_token(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.force_refresh_token(id).await {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} Token 已强制刷新",
            id
        )))
        .into_response(),
        Err(e) => e.into_http_response(),
    }
}

/// POST /api/admin/credentials/reset-stats
/// 重置所有凭据的 success_count
pub async fn reset_all_success_count(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.reset_success_count(None) {
        Ok(count) => Json(SuccessResponse::new(format!(
            "已重置 {} 个凭据的 success_count",
            count
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/reset-stats
/// 重置指定凭据的 success_count
pub async fn reset_success_count(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.reset_success_count(Some(id)) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} success_count 已重置",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/proxy-pool
/// 获取代理池列表
pub async fn get_proxy_pool(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_proxy_pool();
    Json(response)
}

/// POST /api/admin/proxy-pool
/// 添加代理到池中
pub async fn add_proxy(
    State(state): State<AdminState>,
    Json(payload): Json<AddProxyRequest>,
) -> impl IntoResponse {
    match state.service.add_proxy(payload.url, payload.label) {
        Ok(entry) => Json(entry).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxy-pool/batch
/// 批量添加代理
pub async fn batch_add_proxies(
    State(state): State<AdminState>,
    Json(payload): Json<BatchAddProxyRequest>,
) -> impl IntoResponse {
    let (added, errors) = state.service.batch_add_proxies(payload);
    Json(serde_json::json!({
        "added": added.len(),
        "errors": errors.len(),
        "proxies": added,
        "errorMessages": errors
    }))
}

/// DELETE /api/admin/proxy-pool/:id
/// 删除代理
pub async fn delete_proxy(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_proxy(id) {
        Ok(_) => Json(SuccessResponse::new(format!("代理 #{} 已删除", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxy-pool/:id/enabled
/// 设置代理启用/禁用
pub async fn set_proxy_enabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    match state.service.set_proxy_enabled(id, enabled) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "代理 #{} 已{}",
            id,
            if enabled { "启用" } else { "禁用" }
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/proxy
/// 将代理池中的代理分配给凭据
pub async fn assign_proxy_to_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<AssignProxyRequest>,
) -> impl IntoResponse {
    match state.service.assign_proxy_to_credential(id, payload) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 代理已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxy-pool/:id/check
/// 即时探测单个代理的连通性
pub async fn check_proxy(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.check_proxy(id).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxy-pool/check-all
/// 触发全部代理的健康检查
pub async fn check_all_proxies(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.check_all_proxies().await)
}

/// POST /api/admin/proxy-pool/assign-round-robin
/// 将可用代理轮询批量分配给凭据
pub async fn assign_proxies_round_robin(
    State(state): State<AdminState>,
    Json(payload): Json<AssignRoundRobinRequest>,
) -> impl IntoResponse {
    match state
        .service
        .assign_proxies_round_robin(payload.credential_ids)
    {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/load-balancing
/// 获取负载均衡模式
pub async fn get_load_balancing_mode(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_load_balancing_mode();
    Json(response)
}

/// PUT /api/admin/config/load-balancing
/// 设置负载均衡模式
pub async fn set_load_balancing_mode(
    State(state): State<AdminState>,
    Json(payload): Json<SetLoadBalancingModeRequest>,
) -> impl IntoResponse {
    match state.service.set_load_balancing_mode(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/account-throttle
/// 获取账号级风控故障转移配置
pub async fn get_account_throttle_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_account_throttle_config())
}

/// PUT /api/admin/config/account-throttle
/// 更新账号级风控故障转移配置
pub async fn set_account_throttle_config(
    State(state): State<AdminState>,
    Json(payload): Json<SetAccountThrottleConfigRequest>,
) -> impl IntoResponse {
    match state.service.set_account_throttle_config(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/account-rpm-limit
/// 获取单账号 RPM 限流配置
pub async fn get_account_rpm_limit_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_account_rpm_limit_config())
}

/// PUT /api/admin/config/account-rpm-limit
/// 更新单账号 RPM 限流配置
pub async fn set_account_rpm_limit_config(
    State(state): State<AdminState>,
    Json(payload): Json<SetAccountRpmLimitConfigRequest>,
) -> impl IntoResponse {
    match state.service.set_account_rpm_limit_config(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/self-heal
/// 获取自愈治理配置
pub async fn get_self_heal_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_self_heal_config())
}

/// PUT /api/admin/config/self-heal
/// 更新自愈治理配置（运行时生效 + 持久化 config.json）
pub async fn set_self_heal_config(
    State(state): State<AdminState>,
    Json(payload): Json<SetSelfHealConfigRequest>,
) -> impl IntoResponse {
    match state.service.set_self_heal_config(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/log-governance
/// 获取日志治理配置（trace 开关 / trace 保留 / usage 保留）
pub async fn get_log_governance_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_log_governance_config())
}

/// PUT /api/admin/config/log-governance
/// 更新日志治理配置（运行时生效 + 持久化 config.json）
pub async fn set_log_governance_config(
    State(state): State<AdminState>,
    Json(payload): Json<SetLogGovernanceConfigRequest>,
) -> impl IntoResponse {
    match state.service.set_log_governance_config(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/auth/idc/start
/// 发起 IdC 设备授权登录
pub async fn start_idc_login(
    State(state): State<AdminState>,
    Json(payload): Json<StartIdcLoginRequest>,
) -> impl IntoResponse {
    match state.service.start_idc_login(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/auth/idc/poll/:session_id
/// 轮询 IdC 登录状态（由前端按 poll_interval 调用）
pub async fn poll_idc_login(
    State(state): State<AdminState>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    match state.service.poll_idc_login(&session_id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/auth/social/start
/// 发起 Social 登录，返回 portal URL
pub async fn start_social_login(
    State(state): State<AdminState>,
    Json(payload): Json<StartSocialLoginRequest>,
) -> impl IntoResponse {
    match state.service.start_social_login(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/auth/social/poll/:session_id
/// 轮询 Social 登录状态
pub async fn poll_social_login(
    State(state): State<AdminState>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    match state.service.poll_social_login(&session_id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/auth/social/complete/:session_id
///
/// 远程访问场景下手动完成 Social 登录：
/// 用户从浏览器地址栏复制 OAuth 回调 URL，前端提取 code/state/login_option 后调用此接口。
pub async fn complete_social_login(
    State(state): State<AdminState>,
    Path(session_id): Path<String>,
    Json(payload): Json<CompleteSocialLoginRequest>,
) -> impl IntoResponse {
    match state
        .service
        .complete_social_login(
            &session_id,
            payload.code,
            payload.state,
            payload.login_option,
            payload.path,
        )
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/global-proxy
/// 获取当前全局代理配置
pub async fn get_global_proxy(State(state): State<AdminState>) -> impl IntoResponse {
    Json(GlobalProxyResponse {
        proxy_url: state.service.get_global_proxy(),
    })
}

/// PUT /api/admin/config/global-proxy
/// 设置或清除全局代理配置
pub async fn set_global_proxy(
    State(state): State<AdminState>,
    Json(payload): Json<SetGlobalProxyRequest>,
) -> impl IntoResponse {
    match state.service.set_global_proxy(payload.proxy_url) {
        Ok(_) => Json(SuccessResponse::new("全局代理已更新")).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/update
/// 获取在线更新配置（不回显 GitHub Token 明文）
pub async fn get_update_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_update_config())
}

/// PUT /api/admin/config/update
/// 设置在线更新配置
pub async fn set_update_config(
    State(state): State<AdminState>,
    Json(payload): Json<SetUpdateConfigRequest>,
) -> impl IntoResponse {
    match state.service.set_update_config(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/system/update/pull
/// 下载新版二进制并校验（不替换当前进程）
pub async fn pull_update_image(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.pull_update_image().await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/system/update/apply
/// 下载新版二进制、替换 exe，进程退出由容器重启策略接管
pub async fn apply_image_update(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.apply_image_update().await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/system/update/rollback
/// 用 `<exe>.backup` 还原可执行文件并退出进程
pub async fn rollback_image_update(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.rollback_image_update().await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/system/update/check?force=true
/// 查询 GitHub Releases 是否有新版本（带 30 分钟缓存）
pub async fn check_update(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let force = matches!(params.get("force").map(String::as_str), Some("true" | "1"));
    let info = state.service.check_update(force).await;
    Json(info).into_response()
}

/// POST /api/admin/system/update/rate-limit
/// 查询 GitHub API 当前限流配额（可附带 token 用于"保存前先验证"）
pub async fn check_rate_limit(
    State(state): State<AdminState>,
    payload: Option<Json<super::types::CheckRateLimitRequest>>,
) -> impl IntoResponse {
    let req = payload.map(|Json(p)| p).unwrap_or_default();
    let info = state.service.check_rate_limit(req).await;
    Json(info).into_response()
}

/// POST /api/admin/credentials/:id/relogin/social/start
/// 发起 Social 重新登录（更新已有凭据的 Token 而非创建新凭据）
pub async fn start_social_relogin(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<StartSocialLoginRequest>,
) -> impl IntoResponse {
    match state.service.start_social_relogin(id, payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/relogin/social/poll/:session_id
/// 轮询 Social 重新登录状态
pub async fn poll_social_relogin(
    State(state): State<AdminState>,
    Path((_, session_id)): Path<CredSessionPath>,
) -> impl IntoResponse {
    match state.service.poll_social_login(&session_id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/relogin/social/complete/:session_id
/// 远程模式下手动完成 Social 重新登录
pub async fn complete_social_relogin(
    State(state): State<AdminState>,
    Path((_, session_id)): Path<CredSessionPath>,
    Json(payload): Json<CompleteSocialLoginRequest>,
) -> impl IntoResponse {
    match state
        .service
        .complete_social_login(
            &session_id,
            payload.code,
            payload.state,
            payload.login_option,
            payload.path,
        )
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/relogin/idc/start
/// 发起 IdC 重新登录（更新已有凭据的 Token 而非创建新凭据）
pub async fn start_idc_relogin(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<StartIdcLoginRequest>,
) -> impl IntoResponse {
    match state.service.start_idc_relogin(id, payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/relogin/idc/poll/:session_id
/// 轮询 IdC 重新登录状态
pub async fn poll_idc_relogin(
    State(state): State<AdminState>,
    Path((_, session_id)): Path<CredSessionPath>,
) -> impl IntoResponse {
    match state.service.poll_idc_login(&session_id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// PUT /api/admin/config/admin-key
/// 修改登录API密钥（adminApiKey）并持久化到配置文件。
/// 该 key 用于管理面板登录，修改后立即生效。
pub async fn update_admin_key(
    State(state): State<AdminState>,
    Json(payload): Json<UpdateAdminKeyRequest>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    let new_key = payload.new_key.trim().to_string();
    if new_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(super::types::AdminErrorResponse::invalid_request(
                "新登录API密钥不能为空",
            )),
        )
            .into_response();
    }

    // 更新内存中的登录API密钥
    *state.admin_api_key.write() = new_key.clone();

    // 通过 service 持久化到 config.json（从磁盘加载最新后再写，避免覆盖其他字段）
    state.service.persist_admin_key(&new_key);

    Json(SuccessResponse::new("登录API密钥已更新")).into_response()
}

// ============ 客户端 API Key 分发 ============

fn key_to_item(k: &super::client_keys::ClientKey) -> ClientKeyItem {
    ClientKeyItem {
        id: k.id,
        masked_key: mask_client_key(&k.key),
        name: k.name.clone(),
        description: k.description.clone(),
        disabled: k.disabled,
        created_at: k.created_at.clone(),
        last_used_at: k.last_used_at.clone(),
        total_calls: k.total_calls,
        total_input_tokens: k.total_input_tokens,
        total_output_tokens: k.total_output_tokens,
        total_cache_creation_tokens: k.total_cache_creation_tokens,
        total_cache_read_tokens: k.total_cache_read_tokens,
        group: k.group.clone(),
        is_system: k.is_system,
        billing_discount: k.billing_discount,
        billing_price_per_credit: k.billing_price_per_credit,
    }
}

/// GET /api/admin/client-keys
pub async fn list_client_keys(State(state): State<AdminState>) -> impl IntoResponse {
    let keys = state.client_keys.list();
    let items: Vec<ClientKeyItem> = keys.iter().map(key_to_item).collect();
    Json(ClientKeysResponse {
        total: items.len(),
        keys: items,
    })
}

/// POST /api/admin/client-keys
pub async fn create_client_key(
    State(state): State<AdminState>,
    Json(payload): Json<CreateClientKeyRequest>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    let name = payload.name.trim();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(super::types::AdminErrorResponse::invalid_request(
                "name 不能为空",
            )),
        )
            .into_response();
    }
    let entry = state.client_keys.create(
        name.to_string(),
        payload
            .description
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty()),
        payload
            .group
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty()),
    );
    Json(CreateClientKeyResponse {
        id: entry.id,
        key: entry.key,
        name: entry.name,
        created_at: entry.created_at,
    })
    .into_response()
}

/// DELETE /api/admin/client-keys/:id
pub async fn delete_client_key(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    if state.client_keys.is_system(id) {
        return (
            StatusCode::CONFLICT,
            Json(super::types::AdminErrorResponse::invalid_request(
                "系统密钥（config.json apiKey）不可删除",
            )),
        )
            .into_response();
    }
    if state.client_keys.delete(id) {
        Json(SuccessResponse::new(format!("Key #{} 已删除", id))).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(super::types::AdminErrorResponse::not_found(format!(
                "Key #{} 不存在",
                id
            ))),
        )
            .into_response()
    }
}

/// PUT /api/admin/client-keys/:id
pub async fn update_client_key(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<UpdateClientKeyRequest>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    let description = payload
        .description
        .map(|d| if d.is_empty() { None } else { Some(d) });
    let group = payload.group.map(|g| {
        let t = g.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    });
    // 折扣：>0 生效，<=0 视为清除定价（Option<Option<f64>>：外层"是否改动"，内层"设成什么"）
    //
    // 上界不能省。中文里"6 折"和折扣系数 0.6 差 10 倍、和 0.06 差 100 倍，
    // 输入框里一个数字填错量级，客户当月账单就是 10~100 倍——而毛利率会显示
    // 99%，看起来一切正常。宁可 400 拒绝，也不能让它静默进账单。
    const MAX_DISCOUNT: f64 = 1.0; // 卖价高于官方牌价在本业务里不存在
    let billing_discount = payload.billing_discount.map(|v| (v > 0.0).then_some(v));
    if let Some(Some(v)) = billing_discount {
        if !v.is_finite() || v > MAX_DISCOUNT {
            return stats_bad_request(format!(
                "折扣系数须在 (0, {}] 之间，收到 {}。注意：折扣系数不是「几折」——6 折应填 0.6，0.6 折应填 0.006。",
                MAX_DISCOUNT, v
            ));
        }
    }
    // 对客单价上界取成本单价的 20 倍：正常差价在个位数倍数，20 倍以上必是量级填错
    let max_price_per_credit = state.pricing.credit_usd_rate() * 20.0;
    let price_per_credit = payload
        .billing_price_per_credit
        .map(|v| (v > 0.0).then_some(v));
    if let Some(Some(v)) = price_per_credit {
        if !v.is_finite() || v > max_price_per_credit {
            return stats_bad_request(format!(
                "对客单价须在 (0, {:.4}] 美元/credit 之间，收到 {}。我方成本是 {:.4} 美元/credit，超过 20 倍多半是小数点填错了。",
                max_price_per_credit, v, state.pricing.credit_usd_rate()
            ));
        }
    }
    if state.client_keys.update_meta(
        id,
        payload.name,
        description,
        group,
        billing_discount,
        price_per_credit,
    ) {
        Json(SuccessResponse::new(format!("Key #{} 已更新", id))).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(super::types::AdminErrorResponse::not_found(format!(
                "Key #{} 不存在",
                id
            ))),
        )
            .into_response()
    }
}

/// POST /api/admin/client-keys/:id/disabled
pub async fn set_client_key_disabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetDisabledRequest>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    if state.client_keys.set_disabled(id, payload.disabled) {
        let action = if payload.disabled { "禁用" } else { "启用" };
        Json(SuccessResponse::new(format!("Key #{} 已{}", id, action))).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(super::types::AdminErrorResponse::not_found(format!(
                "Key #{} 不存在",
                id
            ))),
        )
            .into_response()
    }
}

/// POST /api/admin/client-keys/:id/reset-stats
pub async fn reset_client_key_stats(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    if state.client_keys.reset_stats(id) {
        Json(SuccessResponse::new(format!("Key #{} 统计已重置", id))).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(super::types::AdminErrorResponse::not_found(format!(
                "Key #{} 不存在",
                id
            ))),
        )
            .into_response()
    }
}

/// POST /api/admin/client-keys/:id/rotate
///
/// 轮换 Key 值：旧明文立即失效，生成新明文返回（仅此一次可见）。
/// 保留 id/name/description/group/统计/disabled 不变，无需重新分组绑定。
pub async fn rotate_client_key(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    match state.client_keys.rotate(id) {
        Some(entry) => {
            // 避免重启时被 config.apiKey 中的旧值覆盖。
            if entry.is_system {
                state.service.persist_api_key(&entry.key);
            }
            Json(CreateClientKeyResponse {
                id: entry.id,
                key: entry.key,
                name: entry.name,
                created_at: entry.created_at,
            })
            .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(super::types::AdminErrorResponse::not_found(format!(
                "Key #{} 不存在",
                id
            ))),
        )
            .into_response(),
    }
}

// ============ 用量统计 ============

fn parse_range(params: &std::collections::HashMap<String, String>) -> Result<Range, String> {
    let Some(range) = params.get("range") else {
        return Err("range 必须是 1h、3h、6h、24h、7d 或 30d".to_string());
    };
    Range::parse(range.as_str())
        .ok_or_else(|| "range 必须是 1h、3h、6h、24h、7d 或 30d".to_string())
}

fn parse_key_id(params: &HashMap<String, String>) -> Result<Option<u64>, String> {
    match params.get("keyId") {
        Some(s) => s
            .parse::<u64>()
            .map(Some)
            .map_err(|_| "keyId 必须是数字".to_string()),
        None => Ok(None),
    }
}

/// 解析可选的分组筛选参数。空字符串视为不传。
fn parse_group_filter(params: &HashMap<String, String>) -> Option<String> {
    params
        .get("group")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 把 group 名转换为该分组下所有凭据 id 的白名单，给 UsageAggregator 用。
/// 返回 None 表示未指定分组（不过滤）；返回 Some(空集) 也是合法值——意味着该分组下没有凭据，
/// 所有 query 都会自然返回空结果。
fn group_to_cred_ids(
    state: &AdminState,
    group: Option<&str>,
) -> Option<std::collections::HashSet<u64>> {
    let g = group?;
    let snapshot = state.service.get_all_credentials();
    Some(
        snapshot
            .credentials
            .iter()
            .filter(|c| c.groups.iter().any(|cg| cg == g))
            .map(|c| c.id)
            .collect(),
    )
}

fn parse_granularity(params: &HashMap<String, String>) -> Result<StatsGranularity, String> {
    match params.get("granularity") {
        Some(s) => {
            StatsGranularity::parse(s).ok_or_else(|| "granularity 必须是 hour 或 day".to_string())
        }
        None => Err("granularity 必须是 hour 或 day".to_string()),
    }
}

fn parse_stats_window(params: &HashMap<String, String>) -> Result<StatsQueryWindow, String> {
    let granularity = parse_granularity(params)?;
    match (params.get("startDate"), params.get("endDate")) {
        (Some(start), Some(end)) => custom_stats_window(start, end, granularity),
        (None, None) => Ok(StatsQueryWindow::preset(parse_range(params)?, granularity)),
        _ => Err("startDate 和 endDate 必须同时提供".to_string()),
    }
}

fn custom_stats_window(
    start: &str,
    end: &str,
    granularity: StatsGranularity,
) -> Result<StatsQueryWindow, String> {
    let start_date = parse_stats_date(start, "startDate")?;
    let end_date = parse_stats_date(end, "endDate")?;
    if end_date < start_date {
        return Err("endDate 不能早于 startDate".to_string());
    }
    let start_ts = local_midnight_ts(start_date)?;
    let end_ts = local_midnight_ts(end_date + Duration::days(1))?;
    Ok(StatsQueryWindow {
        start_ts,
        end_ts,
        granularity,
    })
}

fn parse_stats_date(value: &str, name: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("{} 必须使用 YYYY-MM-DD 格式", name))
}

fn local_midnight_ts(date: NaiveDate) -> Result<i64, String> {
    Local
        .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
        .single()
        .map(|d| d.timestamp())
        .ok_or_else(|| format!("日期 {} 无法转换为本地时间", date))
}

fn stats_query_parts(
    params: &HashMap<String, String>,
) -> Result<(StatsQueryWindow, Option<u64>), String> {
    Ok((parse_stats_window(params)?, parse_key_id(params)?))
}

fn stats_bad_request(message: String) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(super::types::AdminErrorResponse::invalid_request(message)),
    )
        .into_response()
}

/// GET /api/admin/stats/overview
pub async fn stats_overview(State(state): State<AdminState>) -> impl IntoResponse {
    let overview = state.usage_aggregator.overview();
    // 附加：当前活跃 Key / 凭据数
    let active_keys = state.client_keys.active_count() as u64;
    let snapshot = state.service.get_all_credentials();
    let active_credentials = snapshot.credentials.iter().filter(|c| !c.disabled).count() as u64;
    let response = serde_json::json!({
        "todayCalls": overview.today_calls,
        "todayInputTokens": overview.today_input_tokens,
        "todayOutputTokens": overview.today_output_tokens,
        "todayErrors": overview.today_errors,
        "todayCredits": overview.today_credits,
        "weekCalls": overview.week_calls,
        "weekInputTokens": overview.week_input_tokens,
        "weekOutputTokens": overview.week_output_tokens,
        "weekCredits": overview.week_credits,
        "activeClientKeys": active_keys,
        "activeCredentials": active_credentials,
    });
    Json(response)
}

/// GET /api/admin/stats/rate
///
/// 分钟级 RPM / TPM。数据来自内存速率环，**与 trace 开关无关**（关掉 traces.db
/// 不影响这里）。速率取上一个完整分钟，当前分钟仍在累加、读出来会偏低。
///
/// 两个口径都返回，不要混用：
/// - `ingressRpm` 是外部请求数，看真实流量
/// - `upstreamRpm` 是 provider 跳数（含重试与故障转移），看上游压力
/// - 两者比值 `retryAmplification` 就是重试放大倍数
///
/// TPM 同样两个口径：`tpmTotal` 含缓存读取，`tpmBillable` 不含。生产实测这两个数能
/// 差几十倍，所以不能只暴露一个。
pub async fn stats_rate(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let Some(ring) = state.rate_ring.as_ref() else {
        // 未注入速率环的装配（嵌入式/测试）。明确回 503 而不是回一堆 0，
        // 否则前端无法区分"没装采集层"与"真的没流量"。
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(super::types::AdminErrorResponse::invalid_request(
                "速率采集层未启用",
            )),
        )
            .into_response();
    };
    // minutes=N 只取最近 N 分钟；缺省给满环。上面时间范围选 1h 就没必要
    // 传 24 小时的点回去，白白让前端下采样。
    let minutes = params
        .get("minutes")
        .and_then(|v| v.parse::<usize>().ok())
        .map(|n| n.clamp(1, crate::anthropic::rate_ring::RING_MINUTES));
    Json(ring.snapshot_recent(minutes)).into_response()
}

/// GET /api/admin/stats/timeseries?range=24h|7d|30d&granularity=hour|day&group=...
pub async fn stats_timeseries(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let (window, key_id) = match stats_query_parts(&params) {
        Ok(parts) => parts,
        Err(message) => return stats_bad_request(message),
    };
    let group = parse_group_filter(&params);
    let cred_ids = group_to_cred_ids(&state, group.as_deref());
    let points =
        state
            .usage_aggregator
            .query_timeseries(window, key_id, cred_ids.as_ref(), &state.pricing);
    Json(points).into_response()
}

/// GET /api/admin/stats/by-model?range=24h|7d|30d
pub async fn stats_by_model(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let (window, key_id) = match stats_query_parts(&params) {
        Ok(parts) => parts,
        Err(message) => return stats_bad_request(message),
    };
    let data = state
        .usage_aggregator
        .query_by_model(window, key_id, &state.pricing);
    Json(data).into_response()
}

/// GET /api/admin/stats/by-credential?range=24h|7d|30d
pub async fn stats_by_credential(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let (window, key_id) = match stats_query_parts(&params) {
        Ok(parts) => parts,
        Err(message) => return stats_bad_request(message),
    };
    let group = parse_group_filter(&params);
    // 拉一份凭据快照（既给响应附加 email，也用来按 group 构建 cred_ids 白名单，
    // 避免分别查两次）
    let snapshot = state.service.get_all_credentials();
    let email_map: std::collections::HashMap<u64, Option<String>> = snapshot
        .credentials
        .iter()
        .map(|c| (c.id, c.email.clone()))
        .collect();
    let cred_ids: Option<std::collections::HashSet<u64>> = group.as_deref().map(|g| {
        snapshot
            .credentials
            .iter()
            .filter(|c| c.groups.iter().any(|cg| cg == g))
            .map(|c| c.id)
            .collect()
    });
    let data =
        state
            .usage_aggregator
            .query_by_credential(window, key_id, cred_ids.as_ref(), &state.pricing);
    let enriched: Vec<serde_json::Value> = data
        .into_iter()
        .map(|d| {
            let email = email_map.get(&d.credential_id).cloned().flatten();
            serde_json::json!({
                "credentialId": d.credential_id,
                "email": email,
                "calls": d.calls,
                "inputTokens": d.input_tokens,
                "outputTokens": d.output_tokens,
                "cacheCreationTokens": d.cache_creation_tokens,
                "cacheReadTokens": d.cache_read_tokens,
                "errors": d.errors,
                "credits": d.credits,
                "creditUsd": d.credit_usd,
            })
        })
        .collect();
    Json(enriched).into_response()
}

/// GET /api/admin/traces
/// 查询请求链路追踪记录（含每跳明细）。
/// query 参数：status / errorType / credentialId / keyId / group / model / onlyFailed / limit / offset
/// 返回：{ records: [...], total: N }
pub async fn list_traces(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    // 解析分组筛选：把 group 名转为凭据 id 白名单（先于查询执行，避免分页错位）
    let group = params
        .get("group")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let query = match build_trace_query(&state, &params, group.as_deref()) {
        Ok(q) => q,
        Err(message) => return stats_bad_request(message),
    };
    let (records, total) = state.trace_store.query_paged(&query);

    // 附加 credential email 方便前端展示（与 stats_by_credential 一致）
    let snapshot = state.service.get_all_credentials();
    let email_map: HashMap<u64, Option<String>> = snapshot
        .credentials
        .iter()
        .map(|c| (c.id, c.email.clone()))
        .collect();
    let client_key_name_map: HashMap<u64, String> = state
        .client_keys
        .list()
        .into_iter()
        .map(|k| (k.id, k.name))
        .collect();
    // 入口 Key 名称解析：命中客户端 Key 名称表则取名称，否则回退 #id
    // （master apiKey 已下线，历史 key_id=0 记录会显示为 #0）
    let key_label = |key_id: u64| -> String {
        client_key_name_map
            .get(&key_id)
            .cloned()
            .unwrap_or_else(|| format!("#{}", key_id))
    };

    let enriched: Vec<serde_json::Value> = records
        .into_iter()
        .map(|r| {
            let final_email = email_map.get(&r.final_credential_id).cloned().flatten();
            let key_name = key_label(r.key_id);
            // attempts 里每跳也附 email
            let attempts: Vec<serde_json::Value> = r
                .attempts
                .iter()
                .map(|a| {
                    let email = email_map.get(&a.credential_id).cloned().flatten();
                    serde_json::json!({
                        "attempt": a.attempt,
                        "credentialId": a.credential_id,
                        "email": email,
                        "endpoint": a.endpoint,
                        "httpStatus": a.http_status,
                        "outcome": a.outcome,
                        "errorSnippet": a.error_snippet,
                        "durationMs": a.duration_ms,
                    })
                })
                .collect();
            serde_json::json!({
                "traceId": r.trace_id,
                "ts": r.ts,
                "keyId": r.key_id,
                "keySource": r.key_source,
                "keyName": key_name,
                "model": r.model,
                "isStream": r.is_stream,
                "finalStatus": r.final_status,
                "finalCredentialId": r.final_credential_id,
                "finalEmail": final_email,
                "errorType": r.error_type,
                "errorMessage": r.error_message,
                "totalAttempts": r.total_attempts,
                "durationMs": r.duration_ms,
                "interruptedAfterBytes": r.interrupted_after_bytes,
                "inputTokens": r.input_tokens,
                "outputTokens": r.output_tokens,
                "cacheCreationTokens": r.cache_creation_tokens,
                "cacheReadTokens": r.cache_read_tokens,
                "totalTokens": r.input_tokens + r.output_tokens + r.cache_creation_tokens + r.cache_read_tokens,
                "credits": r.credits,
                "creditUsd": state.pricing.credit_usd(r.credits),
                "officialUsd": state.pricing.official_usd(
                    &r.model,
                    r.input_tokens,
                    r.output_tokens,
                    r.cache_creation_tokens,
                    r.cache_read_tokens,
                ),
                "firstTokenMs": r.first_token_ms,
                "attempts": attempts,
            })
        })
        .collect();
    Json(serde_json::json!({ "records": enriched, "total": total })).into_response()
}

/// 从 query 参数构建 [`TraceQuery`]。
///
/// 列表、汇总、TPM 三个端点共用这一份解析，保证「同一组筛选参数 → 同一个
/// WHERE」，不会出现列表和汇总口径漂移。`startDate`/`endDate`（YYYY-MM-DD，
/// 本地时区、endDate 含当天）与 stats 系端点同一套语义。
fn build_trace_query(
    state: &AdminState,
    params: &HashMap<String, String>,
    group: Option<&str>,
) -> Result<TraceQuery, String> {
    let credential_ids: Option<Vec<u64>> = group.map(|g| {
        state
            .service
            .get_all_credentials()
            .credentials
            .iter()
            .filter(|c| c.groups.iter().any(|cg| cg == g))
            .map(|c| c.id)
            .collect()
    });
    // 时间窗有两种写法，与 stats 系保持一致：
    //   range=1h|3h|6h|24h|7d|30d  → 精确时间戳（预设档位用这个）
    //   startDate + endDate        → 自定义区间，按整日展开
    //
    // 从前这里**只认后者**。前端预设档位改发 range 之后，这里静默落到
    // (None, None)，再被调用方兜底成「最近 24 小时」——于是选 7 天和选 24 小时
    // 得到同一个数，而隔壁按聚合器算的面板是对的，两个面板差 34%。
    let (start_ts, end_ts) = match (
        params.get("range"),
        params.get("startDate"),
        params.get("endDate"),
    ) {
        (Some(range), _, _) => {
            let r = crate::admin::usage_stats::Range::parse(range.as_str())
                .ok_or_else(|| "range 必须是 1h、3h、6h、24h、7d 或 30d".to_string())?;
            let w = crate::admin::usage_stats::StatsQueryWindow::preset(
                r,
                crate::admin::usage_stats::StatsGranularity::Hour,
            );
            (Some(w.start_ts), Some(w.end_ts))
        }
        (None, Some(start), Some(end)) => {
            let start_date = parse_stats_date(start, "startDate")?;
            let end_date = parse_stats_date(end, "endDate")?;
            if end_date < start_date {
                return Err("endDate 不能早于 startDate".to_string());
            }
            (
                Some(local_midnight_ts(start_date)?),
                Some(local_midnight_ts(end_date + Duration::days(1))?),
            )
        }
        (None, None, None) => (None, None),
        _ => return Err("startDate 和 endDate 必须同时提供".to_string()),
    };
    Ok(TraceQuery {
        status: params.get("status").filter(|s| !s.is_empty()).cloned(),
        error_type: params.get("errorType").filter(|s| !s.is_empty()).cloned(),
        credential_id: params
            .get("credentialId")
            .and_then(|s| s.parse::<u64>().ok()),
        key_id: params.get("keyId").and_then(|s| s.parse::<u64>().ok()),
        failed_attempt_credential_id: params
            .get("failedAttemptCredentialId")
            .and_then(|s| s.parse::<u64>().ok()),
        model: params.get("model").filter(|s| !s.is_empty()).cloned(),
        only_failed: params
            .get("onlyFailed")
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false),
        credential_ids,
        start_ts,
        end_ts,
        limit: params
            .get("limit")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(crate::admin::trace_db::DEFAULT_QUERY_LIMIT)
            .min(1000),
        offset: params
            .get("offset")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0),
    })
}

/// GET /api/admin/traces/summary
/// 与 /traces 同一套筛选参数（limit/offset 不参与），按模型汇总当前筛选下的
/// 用量与成本，另附合计行。给「请求日志」页的汇总条 + 模型折扣视图用。
pub async fn traces_summary(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let group = params
        .get("group")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let query = match build_trace_query(&state, &params, group.as_deref()) {
        Ok(q) => q,
        Err(message) => return stats_bad_request(message),
    };
    let rows = state.trace_store.summarize_by_model(&query);

    let mut total_calls = 0u64;
    let mut total_errors = 0u64;
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cache_creation = 0u64;
    let mut total_cache_read = 0u64;
    let mut total_credits = 0.0f64;
    let mut total_official = 0.0f64;
    let mut priced_any = false;

    let models: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let credit_usd = state.pricing.credit_usd(r.credits);
            let official_usd = state.pricing.official_usd(
                &r.model,
                r.input_tokens,
                r.output_tokens,
                r.cache_creation_tokens,
                r.cache_read_tokens,
            );
            total_calls += r.calls;
            total_errors += r.errors;
            total_input += r.input_tokens;
            total_output += r.output_tokens;
            total_cache_creation += r.cache_creation_tokens;
            total_cache_read += r.cache_read_tokens;
            total_credits += r.credits;
            if let Some(usd) = official_usd {
                total_official += usd;
                priced_any = true;
            }
            serde_json::json!({
                "model": r.model,
                "calls": r.calls,
                "errors": r.errors,
                "inputTokens": r.input_tokens,
                "outputTokens": r.output_tokens,
                "cacheCreationTokens": r.cache_creation_tokens,
                "cacheReadTokens": r.cache_read_tokens,
                "credits": r.credits,
                "creditUsd": credit_usd,
                "officialUsd": official_usd,
                "discountRatio": crate::common::pricing::discount_ratio(credit_usd, official_usd),
            })
        })
        .collect();

    let total_credit_usd = state.pricing.credit_usd(total_credits);
    let total_official_usd = priced_any.then_some(total_official);
    Json(serde_json::json!({
        "models": models,
        "totals": {
            "calls": total_calls,
            "errors": total_errors,
            "inputTokens": total_input,
            "outputTokens": total_output,
            "cacheCreationTokens": total_cache_creation,
            "cacheReadTokens": total_cache_read,
            "credits": total_credits,
            "creditUsd": total_credit_usd,
            "officialUsd": total_official_usd,
            "discountRatio": crate::common::pricing::discount_ratio(total_credit_usd, total_official_usd),
        },
        "creditUsdRate": state.pricing.credit_usd_rate(),
    }))
    .into_response()
}

/// GET /api/admin/billing/export?keyId=N&month=YYYY-MM[&format=csv]
///
/// 导出某个客户端 Key 的月度请求明细，用于和客户逐条对账。
///
/// 每行是一次真实请求：时间、模型、四类 token、credit、成本、按该 Key 的对客定价
/// 算出的应收。默认 CSV（Excel 直接打开），`format=json` 给程序化对账用。
///
/// 数据源是 `usage_log.*.jsonl`（保留 31 天），逐行流式读取。缺失的日期会在
/// 响应头 `X-Missing-Days` 里列出——"那天没有日志"和"那天没有消费"是两回事，
/// 对账时必须能区分。
pub async fn billing_export(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let Some(key_id) = params.get("keyId").and_then(|s| s.parse::<u64>().ok()) else {
        return stats_bad_request("keyId 必须提供且为数字".to_string());
    };
    let (start, end) = match params.get("month") {
        Some(m) => match month_dates(m) {
            Ok(v) => v,
            Err(message) => return stats_bad_request(message),
        },
        None => {
            let today = Local::now().date_naive();
            let first = today.with_day(1).unwrap_or(today);
            let next = first
                .checked_add_months(chrono::Months::new(1))
                .unwrap_or(today);
            (first, next)
        }
    };

    let Some(recorder) = state.service.usage_recorder() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(super::types::AdminErrorResponse::invalid_request(
                "用量日志未启用，无法导出",
            )),
        )
            .into_response();
    };

    let keys = state.client_keys.list();
    let key = keys.iter().find(|k| k.id == key_id);
    let key_name = key.map(|k| k.name.clone()).unwrap_or_default();
    let discount = key.and_then(|k| k.billing_discount);
    let price_per_credit = key.and_then(|k| k.billing_price_per_credit);

    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut csv = String::from(
        "时间(北京),模型,输入token,输出token,缓存写token,缓存读token,金额USD,状态\n",
    );
    let mut total_credits = 0.0f64;
    let mut total_cost = 0.0f64;
    // 对客金额一律走整数微美元，保证明细能加平合计
    let mut total_receivable_micros = 0i64;
    let want_json = params.get("format").map(|f| f == "json").unwrap_or(false);

    let scan = crate::admin::usage_stats::scan_usage_records(
        recorder.dir(),
        start,
        end,
        Some(key_id),
        |rec| {
            // 明细行的时间按北京时间显示——账期是北京时间算的，明细也必须是，
            // 否则客户按自己那边的日期筛选会发现月头月尾对不上。
            let ts_local = chrono::DateTime::parse_from_rfc3339(&rec.ts)
                .map(|t| {
                    t.with_timezone(&crate::admin::usage_stats::settlement_tz())
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_else(|_| rec.ts.clone());
            // 与总账共用同一个清洗函数，否则两边会对不上（总账清洗、明细不清洗）
            let credits = crate::admin::usage_stats::sane_credits(rec.credits);
            let cost = state.pricing.credit_usd(credits);
            // 单价口径可靠（credits 是上游真值）；折扣口径的分母依赖 token 估算。
            // 失败请求不按牌价收钱：它 credits=0（我方无成本），而 token 是估算的。
            let receivable = match price_per_credit {
                Some(p) => Some(credits * p),
                // 只在折扣口径下把失败行记 0。完全没配定价的 Key 不该出现
                // "一半空白一半 0.000000"的列——客户会问 0 是免费还是没算。
                None if discount.is_some() && rec.status != "success" => Some(0.0),
                None => discount.and_then(|d| {
                    // 与总账共用同一个历史补偿系数，否则明细和总账会对不上
                    let scale = chrono::DateTime::parse_from_rfc3339(&rec.ts)
                        .map(|t| {
                            crate::common::pricing::historical_token_scale(
                                &rec.model,
                                t.timestamp(),
                            )
                        })
                        .unwrap_or(1.0);
                    let up = |v: u64| if scale == 1.0 { v } else { (v as f64 * scale) as u64 };
                    // 与总账共用同一条 websearch 缓存还原，否则明细加不平合计
                    let ts_secs = chrono::DateTime::parse_from_rfc3339(&rec.ts)
                        .map(|t| t.timestamp())
                        .unwrap_or(i64::MAX);
                    let (input_tokens, cache_read_tokens) =
                        crate::common::pricing::websearch_cache_correction(
                            &rec.model,
                            ts_secs,
                            rec.input_tokens,
                            rec.cache_creation_tokens,
                            rec.cache_read_tokens,
                        );
                    state
                        .pricing
                        .official_usd(
                            &rec.model,
                            up(input_tokens),
                            rec.output_tokens,
                            up(rec.cache_creation_tokens),
                            up(cache_read_tokens),
                        )
                        .map(|o| o * d)
                }),
            };
            // 合计必须由**打印出去的那些数**加出来。若合计用未舍入值、行用舍入值，
            // 客户把一列加一遍就会对不上——对账单里最经不起的就是这个。
            total_credits += round6(credits);
            total_cost += round6(cost);
            let receivable_micros = receivable.map(to_micros);
            total_receivable_micros += receivable_micros.unwrap_or(0);

            if want_json {
                rows.push(serde_json::json!({
                    "ts": ts_local,
                    "model": rec.model,
                    "inputTokens": rec.input_tokens,
                    "outputTokens": rec.output_tokens,
                    "cacheCreationTokens": rec.cache_creation_tokens,
                    "cacheReadTokens": rec.cache_read_tokens,
                    "credits": credits,
                    "costUsd": cost,
                    "receivableUsd": receivable,
                    "status": rec.status,
                }));
            } else {
                use std::fmt::Write as _;
                // 对客明细只出「金额」= 应收。credit 是我方与上游的结算单位，
                // 成本更是我方内部数字，都不该出现在给客户的对账单上。
                let _ = writeln!(
                    csv,
                    "{},{},{},{},{},{},{},{}",
                    csv_field(&ts_local),
                    // model 是客户端原样传进来的，不转义就能用一个逗号把后面
                    // 每一列右移一位——金额会落进「状态」列
                    csv_field(&rec.model),
                    rec.input_tokens,
                    rec.output_tokens,
                    rec.cache_creation_tokens,
                    rec.cache_read_tokens,
                    // 打印的就是累加的那个整数，不存在第二条舍入路径
                    receivable_micros
                        .map(fmt_micros)
                        .unwrap_or_else(|| "".to_string()),
                    csv_field(&rec.status),
                );
            }
        },
    );

    let scanned = scan.scanned;
    let missing = scan.missing_days;
    // 口径判定要和总账一致：配了折扣但当期全部流量都落在未配价模型上时，
    // 总账判 "无法计价"，导出这边不能自称 discount 还给个 0.0。
    let basis = if price_per_credit.is_some() {
        "perCredit"
    } else if discount.is_some() && total_receivable_micros > 0 {
        "discount"
    } else {
        "none"
    };

    if want_json {
        return Json(serde_json::json!({
            "keyId": key_id,
            "keyName": key_name,
            "month": params.get("month"),
            "rows": rows,
            "malformedLines": scan.malformed,
            "totals": {
                "records": scanned,
                "credits": total_credits,
                "costUsd": total_cost,
                "receivableUsd": (basis != "none")
                    .then_some(total_receivable_micros as f64 / 1e6),
            },
            "receivableBasis": basis,
            "missingDays": missing,
        }))
        .into_response();
    }

    // 汇总行随表一起给出去，客户拿到的就是一张能直接核的账
    use std::fmt::Write as _;
    let _ = writeln!(
        csv,
        "\"合计\",\"{} 条\",,,,,{},",
        scanned,
        if basis != "none" {
            fmt_micros(total_receivable_micros)
        } else {
            String::new()
        }
    );

    // 缺口要写在客户拿到的那份文件里，不能只放在响应头——客户看到的是 CSV
    if !missing.is_empty() {
        let _ = writeln!(
            csv,
            "\"以下日期无用量日志（非零消费）\",\"{}\"",
            missing.join(" ")
        );
    }
    if scan.malformed > 0 {
        let _ = writeln!(
            csv,
            "\"注：本期有 {} 行日志无法解析，金额未计入以上合计\",",
            scan.malformed
        );
    }

    let filename = format!(
        "billing-{}-{}.csv",
        if key_name.is_empty() {
            format!("key{}", key_id)
        } else {
            key_name.replace(['/', ' ', '\\'], "_")
        },
        params
            .get("month")
            .cloned()
            .unwrap_or_else(|| start.format("%Y-%m").to_string())
    );

    // BOM：没有它 Excel 会把中文表头显示成乱码
    let body = format!("\u{feff}{}", csv);
    let mut resp = body.into_response();
    let headers = resp.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
        "attachment; filename*=UTF-8''{}",
        urlencoding::encode(&filename)
    )) {
        headers.insert(axum::http::header::CONTENT_DISPOSITION, v);
    }
    if !missing.is_empty() {
        if let Ok(v) = axum::http::HeaderValue::from_str(&missing.join(",")) {
            headers.insert("x-missing-days", v);
        }
    }
    if scan.malformed > 0 {
        if let Ok(v) = axum::http::HeaderValue::from_str(&scan.malformed.to_string()) {
            headers.insert("x-malformed-lines", v);
        }
    }
    resp
}

/// CSV 字段转义（RFC4180）+ 防 Excel 公式注入。
///
/// model / status 都可能带客户端可控内容，不转义会顶歪整行的列对齐——
/// 客户把应收那一列加一遍就和合计对不上，对账单最经不起这个。
fn csv_field(v: &str) -> String {
    let needs_prefix = v.starts_with(['=', '+', '-', '@']);
    let escaped = v.replace('"', "\"\"");
    if needs_prefix {
        format!("\"'{}\"", escaped)
    } else {
        format!("\"{}\"", escaped)
    }
}

/// 舍入到 6 位小数（仅供 JSON 口径使用）。
fn round6(v: f64) -> f64 {
    if v.is_finite() {
        (v * 1e6).round() / 1e6
    } else {
        0.0
    }
}

/// 金额转「微美元」整数（6 位小数的最小单位）。
///
/// CSV 的每一行和合计行**必须来自同一个整数**，否则行用 `{:.6}` 格式化、合计用
/// 浮点累加，两条舍入路径会在半分位上分叉——实测 11490 行差了 0.000055，客户
/// 把金额列加一遍就对不上合计。整数累加让"加得平"由构造保证，而不是靠两个
/// 舍入函数碰巧一致。
fn to_micros(v: f64) -> i64 {
    if v.is_finite() {
        (v * 1e6).round() as i64
    } else {
        0
    }
}

/// 微美元整数还原成 6 位小数字符串（与 `to_micros` 严格互逆）
fn fmt_micros(m: i64) -> String {
    let sign = if m < 0 { "-" } else { "" };
    let a = m.abs();
    format!("{}{}.{:06}", sign, a / 1_000_000, a % 1_000_000)
}

#[cfg(test)]
mod money_tests {
    use super::*;

    /// 明细逐行加总必须精确等于合计——这是整张对账单的立身之本。
    /// 从前行用 `{:.6}` 格式化、合计用浮点累加，两条舍入路径在半分位上分叉，
    /// 实测 11490 行差了 0.000055，客户把金额列加一遍就对不上。
    #[test]
    fn printed_rows_sum_exactly_to_the_printed_total() {
        // 刻意挑落在半微美元边界上的值——正是从前分叉的那一类
        let values = [
            0.0212054999_f64,
            0.0000005,
            0.1234565,
            1.9999995,
            0.000_000_4,
            642.532_866_5,
        ];
        let mut total = 0i64;
        let mut printed_sum = 0i64;
        for v in values {
            let m = to_micros(v);
            total += m;
            // 客户看到的是字符串，所以就从字符串反解回来加
            let s = fmt_micros(m);
            let parsed: f64 = s.parse().unwrap();
            printed_sum += (parsed * 1e6).round() as i64;
        }
        assert_eq!(
            printed_sum, total,
            "打印出去的数加起来必须等于合计，实得 {} vs {}",
            fmt_micros(printed_sum),
            fmt_micros(total)
        );
    }

    #[test]
    fn fmt_micros_is_the_inverse_of_to_micros() {
        for m in [0i64, 1, -1, 999_999, 1_000_000, -1_234_567, 642_532_866] {
            let s = fmt_micros(m);
            assert_eq!(to_micros(s.parse::<f64>().unwrap()), m, "往返不一致: {}", s);
        }
    }

    /// CSV 字段必须转义：model 是客户端可控的，一个逗号就能把金额顶进状态列。
    #[test]
    fn csv_fields_survive_hostile_model_names() {
        assert_eq!(csv_field("claude-opus-4-5,999"), "\"claude-opus-4-5,999\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("=1+1"), "\"'=1+1\"");
    }
}

/// `YYYY-MM` → (月初, 次月初) 两个日期
fn month_dates(month: &str) -> Result<(NaiveDate, NaiveDate), String> {
    let parts: Vec<&str> = month.split('-').collect();
    if parts.len() != 2 {
        return Err("month 必须是 YYYY-MM 格式".to_string());
    }
    let year: i32 = parts[0].parse().map_err(|_| "month 年份无效".to_string())?;
    let mon: u32 = parts[1].parse().map_err(|_| "month 月份无效".to_string())?;
    let first = NaiveDate::from_ymd_opt(year, mon, 1).ok_or("month 无效".to_string())?;
    let next = if mon == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, mon + 1, 1)
    }
    .ok_or("month 无效".to_string())?;
    Ok((first, next))
}

/// GET /api/admin/config/scheduling/throughput-estimate
///
/// 开启吞吐模式前先看这个：能提到多少并发、可用额度还能撑多久、
/// 撑到重置的话每分钟只能烧多少 token。
///
/// 「打开吞吐模式」听起来像免费加速，其实只改变流量**怎么分布**，
/// 不改变**能烧多少**——上游额度按月固定，铺开并发只会烧得更快。
/// 所以这个接口存在的意义是：开之前让人看见代价。
pub async fn throughput_estimate(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use crate::admin::scheduling::ThroughputObservations;

    // 观测值从最近的真实用量里算，不写死。
    let (tokens_per_credit, credits_per_hour) = state
        .service
        .usage_recorder()
        .map(|r| observed_burn(r.dir()))
        .unwrap_or((0.0, 0.0));

    let obs = ThroughputObservations {
        // 单凭据并发峰值：无运行时在飞计数（全仓没有并发限制器），
        // 只能取实测观测值。可用 query 覆盖以便做假设推演。
        per_credential_concurrency: params
            .get("perCredentialConcurrency")
            .and_then(|v| v.parse().ok())
            .unwrap_or(OBSERVED_PER_CREDENTIAL_CONCURRENCY),
        tokens_per_credit,
        credits_per_hour,
        hours_to_reset: state.service.hours_to_quota_reset().unwrap_or(0.0),
    };
    let est = state.service.throughput_estimate(obs);
    Json(serde_json::json!({
        "estimate": est,
        "observations": {
            "perCredentialConcurrency": obs.per_credential_concurrency,
            "tokensPerCredit": obs.tokens_per_credit,
            "creditsPerHour": obs.credits_per_hour,
            "hoursToReset": obs.hours_to_reset,
        },
        // 口径说明直接跟着数走，免得数字被单独截图后失去上下文
        "caveat": "并发是能力上限不是保证值；可持续 TPM 由额度决定，与速率无关。提并发不会增加月度总量，只会更快烧完。",
    }))
    .into_response()
}

/// 实测烧速：返回 (每 credit 折合计费 token, credits/小时)。
///
/// 取最近 3 个日志文件——够抹平单日波动，又不至于把很久以前的用法混进来。
fn observed_burn(dir: &std::path::Path) -> (f64, f64) {
    use std::io::BufRead;
    let mut files: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().into_string().ok()?;
                (n.starts_with("usage_log.") && n.ends_with(".jsonl")).then(|| e.path())
            })
            .collect(),
        Err(_) => return (0.0, 0.0),
    };
    files.sort();
    let recent: Vec<_> = files.into_iter().rev().take(3).collect();
    let (mut credits, mut billable, mut earliest, mut latest) = (0.0f64, 0u64, f64::MAX, 0.0f64);
    for path in recent {
        let Ok(f) = std::fs::File::open(&path) else {
            continue;
        };
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            let Ok(rec) =
                serde_json::from_str::<crate::admin::usage_stats::UsageRecord>(line.trim())
            else {
                continue;
            };
            credits += crate::admin::usage_stats::sane_credits(rec.credits);
            billable += rec.input_tokens + rec.output_tokens + rec.cache_creation_tokens;
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&rec.ts) {
                let t = ts.timestamp() as f64;
                earliest = earliest.min(t);
                latest = latest.max(t);
            }
        }
    }
    if credits <= 0.0 {
        return (0.0, 0.0);
    }
    let hours = ((latest - earliest) / 3600.0).max(1.0);
    (billable as f64 / credits, credits / hours)
}

/// 实测的单凭据并发峰值。全仓没有并发限制器（无信号量、无在飞计数），
/// 所以这个数只能来自观测：2026-08 用 ts+durationMs 反推，各凭据峰值 8~38，
/// 取中位附近的 16 作为保守估计。
const OBSERVED_PER_CREDENTIAL_CONCURRENCY: u32 = 16;

/// GET /api/admin/config/scheduling —— 读取凭据调度自动化配置
pub async fn get_scheduling_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.scheduling_config())
}

/// PUT /api/admin/config/scheduling —— 更新配置
pub async fn set_scheduling_config(
    State(state): State<AdminState>,
    Json(payload): Json<crate::admin::scheduling::SchedulingConfig>,
) -> axum::response::Response {
    if !(0.0..=100.0).contains(&payload.demote_threshold_pct) {
        return stats_bad_request("demoteThresholdPct 必须在 0-100 之间".to_string());
    }
    // 降级目标必须真的把凭据推到二线，否则"降级"是个空操作
    if payload.demote_to <= crate::admin::scheduling::PRIORITY_BASELINE {
        return stats_bad_request(format!(
            "demoteTo 必须大于基线 {}，否则降级后仍在主力位置",
            crate::admin::scheduling::PRIORITY_BASELINE
        ));
    }
    match state.service.set_scheduling_config(payload.clone()) {
        Ok(()) => Json(serde_json::json!({
            "success": true,
            "message": "调度配置已更新",
            "config": payload,
        }))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/config/scheduling/run —— 立即跑一轮调度（不等下个周期）
///
/// 返回本轮实际执行的调整，便于在 UI 上直接看到"改了谁、从几改到几、为什么"。
pub async fn run_scheduling_now(State(state): State<AdminState>) -> impl IntoResponse {
    let applied = state.service.run_scheduling_pass();
    let items: Vec<serde_json::Value> = applied
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "from": c.from,
                "to": c.to,
                "reason": c.reason,
            })
        })
        .collect();
    Json(serde_json::json!({
        "applied": items.len(),
        "changes": items,
    }))
}

/// GET /api/admin/billing?month=YYYY-MM（或 startDate/endDate）
///
/// 月度总账：每个入口 Key 的成本、官方牌价、按其对客折扣算出的应收与毛利，外加合计。
///
/// 数据源是聚合器的**天桶**（保留 31 天），不是 traces.db（只留 7 天）——月结必须
/// 覆盖整月。因此结算窗口早于 31 天前的月份查不到数据，响应里给出实际覆盖范围。
pub async fn billing_summary(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    // month=YYYY-MM 是月结的常用写法；也接受与 stats 系一致的 startDate/endDate。
    // 账期一律按**北京时间自然日**——月结口径以此为准。
    let (window_start_date, window_end_date) = if let Some(month) = params.get("month") {
        match month_dates(month) {
            Ok(v) => v,
            Err(message) => return stats_bad_request(message),
        }
    } else {
        match (params.get("startDate"), params.get("endDate")) {
            (Some(s), Some(e)) => {
                let (Ok(sd), Ok(ed)) = (
                    NaiveDate::parse_from_str(s, "%Y-%m-%d"),
                    NaiveDate::parse_from_str(e, "%Y-%m-%d"),
                ) else {
                    return stats_bad_request("startDate/endDate 需为 YYYY-MM-DD".to_string());
                };
                if ed < sd {
                    return stats_bad_request("endDate 不能早于 startDate".to_string());
                }
                // endDate 是闭区间的最后一天，转成开区间
                (sd, ed.succ_opt().unwrap_or(ed))
            }
            _ => {
                // 缺省：当前自然月至今（含今天）
                let today = Local::now().date_naive();
                let first = today.with_day(1).unwrap_or(today);
                (first, today.succ_opt().unwrap_or(today))
            }
        }
    };

    let keys = state.client_keys.list();
    let pricing_map: HashMap<u64, (Option<f64>, Option<f64>)> = keys
        .iter()
        .map(|k| (k.id, (k.billing_discount, k.billing_price_per_credit)))
        .collect();
    let name_map: HashMap<u64, String> = keys.iter().map(|k| (k.id, k.name.clone())).collect();

    // 账目直接从用量日志算，和导出明细同源——客户把导出的每一条加起来，必须等于
    // 这里给出的总数。走内存聚合器会同时踩两个坑：只有 31 个日桶（月初几天会被
    // 静默算成零消费），且与明细是两份数据。
    let (rows, scan) = match state.service.usage_recorder() {
        Some(recorder) => crate::admin::usage_stats::billing_from_logs(
            recorder.dir(),
            window_start_date,
            window_end_date,
            &state.pricing,
            &|id| pricing_map.get(&id).copied().unwrap_or((None, None)),
        ),
        None => (
            Vec::new(),
            crate::admin::usage_stats::ScanOutcome::default(),
        ),
    };

    let mut total_cost = 0.0f64;
    let mut total_official = 0.0f64;
    let mut official_any = false;
    let mut total_receivable = 0.0f64;
    let mut receivable_any = false;
    let mut unpriced_keys: Vec<serde_json::Value> = Vec::new();

    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            total_cost += r.credit_usd;
            if let Some(o) = r.official_usd {
                total_official += o;
                official_any = true;
            }
            match r.receivable_usd {
                Some(v) => {
                    total_receivable += v;
                    receivable_any = true;
                }
                None if r.credit_usd > 0.0 => {
                    // 有消耗却算不出应收 —— 月结时这是必须人工处理的漏收口
                    unpriced_keys.push(serde_json::json!({
                        "keyId": r.key_id,
                        "name": name_map.get(&r.key_id),
                        "costUsd": r.credit_usd,
                        "reason": if r.billing_discount.is_none() && r.price_per_credit.is_none() {
                            "未设置对客定价"
                        } else {
                            "模型未配官方价"
                        },
                    }));
                }
                None => {}
            }
            serde_json::json!({
                "keyId": r.key_id,
                "name": name_map.get(&r.key_id),
                "calls": r.calls,
                "errors": r.errors,
                "errorCredits": r.error_credits,
                "unpricedCalls": r.unpriced_calls,
                "inputTokens": r.input_tokens,
                "outputTokens": r.output_tokens,
                "cacheCreationTokens": r.cache_creation_tokens,
                "cacheReadTokens": r.cache_read_tokens,
                "credits": r.credits,
                "costUsd": r.credit_usd,
                "officialUsd": r.official_usd,
                "billingDiscount": r.billing_discount,
                "pricePerCredit": r.price_per_credit,
                "receivableUsd": r.receivable_usd,
                "receivableBasis": r.receivable_basis,
                "marginUsd": r.margin_usd,
            })
        })
        .collect();

    // 只要该 Key 还有别的模型配了价，official_usd 就是 Some(部分和)，
    // 上面的 unpriced_keys 完全抓不到——但落在未配价模型上的那部分请求
    // 已经静默地从应收里消失了。混合流量必须单独报出来。
    for r in &rows {
        if r.unpriced_calls > 0 && r.receivable_basis == Some("discount") {
            unpriced_keys.push(serde_json::json!({
                "keyId": r.key_id,
                "name": name_map.get(&r.key_id),
                "costUsd": state.pricing.credit_usd(r.unpriced_credits),
                "reason": format!(
                    "{} 次调用的模型未配官方价，这部分未计入应收",
                    r.unpriced_calls
                ),
            }));
        }
    }

    // 有调用但 credits 全是 0：上游改了 meteringEvent 的事件名或字段名时会长这样。
    // 这是全套账里唯一"错了没有指纹"的情形——成本和应收会一起显示 $0，
    // 连 unpricedKeys 都不响（它的条件是成本 > 0）。必须单独兜住。
    let zero_credit_keys: Vec<serde_json::Value> = rows
        .iter()
        // upstream_calls 已剔除本地 WebSearch（不走上游、credits 恒 0），
        // 否则一个只跑 websearch 的 Key 会每月稳定误报，把这条唯一的
        // 红色告警训练成"忽略项"。
        .filter(|r| r.upstream_calls > 0 && r.credits <= 0.0)
        .map(|r| {
            serde_json::json!({
                "keyId": r.key_id,
                "name": name_map.get(&r.key_id),
                "calls": r.upstream_calls,
            })
        })
        .collect();

    let receivable = receivable_any.then_some(total_receivable);
    Json(serde_json::json!({
        "windowStart": window_start_date.format("%Y-%m-%d").to_string(),
        "windowEnd": window_end_date.format("%Y-%m-%d").to_string(),
        "timezone": "Asia/Shanghai",
        "keys": items,
        "totals": {
            "costUsd": total_cost,
            "officialUsd": official_any.then_some(total_official),
            "receivableUsd": receivable,
            "marginUsd": receivable.map(|r| r - total_cost),
            "marginRate": receivable
                .filter(|r| *r > 0.0)
                .map(|r| (r - total_cost) / r * 100.0),
        },
        // 有成本但收不出钱的 Key：月结前应当逐个处理，不能让它们静默进毛利
        "unpricedKeys": unpriced_keys,
        // 有成功调用但 credits 为 0 的 Key：上游计费事件可能已经变了协议
        "zeroCreditKeys": zero_credit_keys,
        "creditUsdRate": state.pricing.credit_usd_rate(),
        // 缺失日期必须露出来：那天没日志 ≠ 那天没消费，月结时要能分辨
        "missingDays": scan.missing_days,
        // 解析不出来的行：金额未知且不可知，>0 就说明这张账单可能不完整
        "malformedLines": scan.malformed,
        "note": "账期按北京时间自然日；数据源为用量日志（保留期见配置），更早的月份查不到",
        // 成本（credits）来自上游 meteringEvent，是真值；官方牌价靠 token 明细换算，
        // 而 token 明细在上游不下发时由本地估算补齐（ARCHITECTURE.md §4.1.1）。
        // 因此按单价计费（perCredit）可靠，按官方价打折（discount）只应作参考。
        "costReliable": true,
        "officialUsdEstimated": true,
    }))
    .into_response()
}

/// GET /api/admin/stats/tpm?dim=key|credential
/// 分维度分钟级 TPM/RPM 统计（数据源：traces.db，旁路查询）。
/// 支持与 /traces 相同的筛选参数（startDate/endDate/keyId/group/model 等）。
///
/// 「峰值 TPM」= 窗口内单分钟最大 token 消耗，是每个 Key/凭据实测承载的
/// 直接证据。trace 被关掉时这里只有历史数据，响应里带 traceEnabled 供前端提示。
pub async fn stats_tpm(
    State(state): State<AdminState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use crate::admin::trace_db::TpmDim;
    let dim = match params.get("dim").map(|s| s.as_str()) {
        Some("key") | None => TpmDim::Key,
        Some("credential") => TpmDim::Credential,
        Some(_) => return stats_bad_request("dim 必须是 key 或 credential".to_string()),
    };
    let group = params
        .get("group")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut query = match build_trace_query(&state, &params, group.as_deref()) {
        Ok(q) => q,
        Err(message) => return stats_bad_request(message),
    };
    // 无时间窗时默认最近 24 小时：trace 库是单连接互斥锁，读查询期间会顶住
    // 请求收尾的 insert。全表分钟聚合实测 ~600ms、24h 窗口 ~50ms，兜底限窗
    // 把对写入路径的最坏占锁控制在几十毫秒级。
    if query.start_ts.is_none() && query.end_ts.is_none() {
        query.start_ts = Some(chrono::Utc::now().timestamp() - 24 * 3600);
    }
    let stats = state.trace_store.tpm_stats(dim, &query);

    // 实体名称解析：Key 维度用客户端 Key 名，凭据维度用 email
    let entities: Vec<serde_json::Value> = match dim {
        TpmDim::Key => {
            let name_map: HashMap<u64, String> = state
                .client_keys
                .list()
                .into_iter()
                .map(|k| (k.id, k.name))
                .collect();
            stats
                .iter()
                .map(|s| {
                    let label = name_map
                        .get(&s.entity_id)
                        .cloned()
                        .unwrap_or_else(|| format!("#{}", s.entity_id));
                    tpm_entity_json(s, label, &state.pricing)
                })
                .collect()
        }
        TpmDim::Credential => {
            let email_map: HashMap<u64, Option<String>> = state
                .service
                .get_all_credentials()
                .credentials
                .iter()
                .map(|c| (c.id, c.email.clone()))
                .collect();
            stats
                .iter()
                .map(|s| {
                    let label = email_map
                        .get(&s.entity_id)
                        .cloned()
                        .flatten()
                        .unwrap_or_else(|| format!("#{}", s.entity_id));
                    tpm_entity_json(s, label, &state.pricing)
                })
                .collect()
        }
    };
    // 全系统合计：按分钟合并全部实体后再取峰值。各实体峰值相加会得到一个
    // 从未真实发生过的数（峰值多半落在不同分钟）。
    let mut totals = state.trace_store.tpm_totals(&query);
    // 逐模型 token 明细从各实体汇总而来——这样合计行的折扣与各行同源，
    // 不会出现"每行都是 0.6 折、合计却是别的数"这种自相矛盾。
    let mut merged: std::collections::HashMap<String, crate::admin::trace_db::ModelTokenSums> =
        std::collections::HashMap::new();
    for s in &stats {
        for m in &s.model_tokens {
            let e = merged
                .entry(m.model.clone())
                .or_insert_with(|| crate::admin::trace_db::ModelTokenSums {
                    model: m.model.clone(),
                    calls: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_read_tokens: 0,
                });
            e.calls += m.calls;
            e.input_tokens += m.input_tokens;
            e.output_tokens += m.output_tokens;
            e.cache_creation_tokens += m.cache_creation_tokens;
            e.cache_read_tokens += m.cache_read_tokens;
        }
    }
    totals.model_tokens = merged.into_values().collect();
    Json(serde_json::json!({
        "dim": match dim { TpmDim::Key => "key", TpmDim::Credential => "credential" },
        "traceEnabled": state.trace_store.is_enabled(),
        "entities": entities,
        "totals": tpm_entity_json(&totals, "全系统".to_string(), &state.pricing),
    }))
    .into_response()
}

fn tpm_entity_json(
    s: &crate::admin::trace_db::TpmEntityStats,
    label: String,
    pricing: &crate::common::pricing::PricingTable,
) -> serde_json::Value {
    let success = s.total_calls.saturating_sub(s.errors);
    // 官方成本必须按模型逐项算（各模型单价不同）；只累计已配价模型，
    // 一个都算不出来时给 None，前端显示"—"而不是把折扣算成免费。
    let mut official = 0.0f64;
    let mut priced_any = false;
    for m in &s.model_tokens {
        if let Some(usd) = pricing.official_usd(
            &m.model,
            m.input_tokens,
            m.output_tokens,
            m.cache_creation_tokens,
            m.cache_read_tokens,
        ) {
            official += usd;
            priced_any = true;
        }
    }
    let official_usd = priced_any.then_some(official);
    let credit_usd = pricing.credit_usd(s.credits);
    serde_json::json!({
        "entityId": s.entity_id,
        "label": label,
        "peakTpmTotal": s.peak_tpm_total,
        "peakTpmBillable": s.peak_tpm_billable,
        "peakRpm": s.peak_rpm,
        "activeMinutes": s.active_minutes,
        "avgTpmActive": s.avg_tpm_active,
        "avgRpmActive": s.avg_rpm_active,
        "totalTokens": s.total_tokens,
        "totalCalls": s.total_calls,
        "errors": s.errors,
        "successRate": if s.total_calls > 0 {
            success as f64 / s.total_calls as f64 * 100.0
        } else {
            0.0
        },
        "credits": s.credits,
        "creditUsd": credit_usd,
        "officialUsd": official_usd,
        "discountRatio": crate::common::pricing::discount_ratio(credit_usd, official_usd),
        "topModel": s.top_model,
        "topModelShare": s.top_model_share,
    })
}

/// GET /api/admin/traces/failure-stats
/// 按凭据聚合失败次数（鉴权 / 账号风控 / 其他三类），用于卡片分色展示。
/// 返回 { "<credentialId>": { auth, throttle, other }, ... }
pub async fn trace_failure_stats(State(state): State<AdminState>) -> impl IntoResponse {
    let stats = state.trace_store.failure_stats();
    let map: std::collections::HashMap<String, serde_json::Value> = stats
        .into_iter()
        .map(|(id, s)| {
            (
                id.to_string(),
                serde_json::json!({
                    "auth": s.auth,
                    "throttle": s.throttle,
                    "other": s.other,
                }),
            )
        })
        .collect();
    Json(map)
}

// ============ 账号分组（独立实体）============

fn group_to_item(g: &super::groups::Group, state: &AdminState) -> super::types::GroupItem {
    super::types::GroupItem {
        name: g.name.clone(),
        description: g.description.clone(),
        created_at: g.created_at.clone(),
        credential_count: state
            .service
            .token_manager()
            .count_credentials_with_group(&g.name),
        client_key_count: state.client_keys.count_with_group(&g.name),
    }
}

/// GET /api/admin/groups
pub async fn list_groups(State(state): State<AdminState>) -> impl IntoResponse {
    let groups = state.groups.list();
    let items: Vec<super::types::GroupItem> =
        groups.iter().map(|g| group_to_item(g, &state)).collect();
    Json(super::types::GroupsResponse {
        total: items.len(),
        groups: items,
    })
}

/// POST /api/admin/groups
pub async fn create_group(
    State(state): State<AdminState>,
    Json(payload): Json<super::types::CreateGroupRequest>,
) -> impl IntoResponse {
    match state.groups.create(payload.name, payload.description) {
        Ok(g) => Json(group_to_item(&g, &state)).into_response(),
        Err(e) => {
            let msg = e.to_string();
            // "已存在" → 409；其他校验失败 → 400
            let (code, resp) = if msg.contains("已存在") {
                (
                    StatusCode::CONFLICT,
                    super::types::AdminErrorResponse::invalid_request(msg),
                )
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    super::types::AdminErrorResponse::invalid_request(msg),
                )
            };
            (code, Json(resp)).into_response()
        }
    }
}

/// PATCH /api/admin/groups/:name
///
/// 改名 / 改备注。改名时级联更新所有引用该分组的凭据 / 客户端 Key。
pub async fn update_group(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    Json(payload): Json<super::types::UpdateGroupRequest>,
) -> impl IntoResponse {
    if !state.groups.exists(&name) {
        return (
            StatusCode::NOT_FOUND,
            Json(super::types::AdminErrorResponse::not_found(format!(
                "分组 {} 不存在",
                name
            ))),
        )
            .into_response();
    }

    // 1. 改名（先校验目标名再级联）
    let mut current_name = name.clone();
    if let Some(new_name) = payload.new_name.as_deref() {
        let trimmed = new_name.trim();
        if !trimmed.is_empty() && trimmed != name {
            // GroupManager 内做唯一性 / 长度 / 空校验
            match state.groups.rename(&name, trimmed) {
                Ok(_) => {}
                Err(e) => {
                    let msg = e.to_string();
                    let code = if msg.contains("已存在") {
                        StatusCode::CONFLICT
                    } else {
                        StatusCode::BAD_REQUEST
                    };
                    return (
                        code,
                        Json(super::types::AdminErrorResponse::invalid_request(msg)),
                    )
                        .into_response();
                }
            }
            // 级联：失败时尝试回滚分组改名（避免注册表与凭据 / Key 不一致）
            let cred_res = state
                .service
                .token_manager()
                .rename_credential_group(&name, trimmed);
            if let Err(e) = cred_res {
                let _ = state.groups.rename(trimmed, &name);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(super::types::AdminErrorResponse::internal_error(format!(
                        "级联更新凭据失败: {}",
                        e
                    ))),
                )
                    .into_response();
            }
            state.client_keys.rename_group(&name, trimmed);
            current_name = trimmed.to_string();
        }
    }

    // 2. 改备注
    if let Some(desc) = payload.description {
        let desc_opt = if desc.trim().is_empty() {
            None
        } else {
            Some(desc)
        };
        if let Err(e) = state.groups.update_description(&current_name, desc_opt) {
            return (
                StatusCode::BAD_REQUEST,
                Json(super::types::AdminErrorResponse::invalid_request(
                    e.to_string(),
                )),
            )
                .into_response();
        }
    }

    let group = match state.groups.get(&current_name) {
        Some(g) => g,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(super::types::AdminErrorResponse::internal_error(
                    "分组在更新过程中消失，状态异常",
                )),
            )
                .into_response();
        }
    };
    Json(group_to_item(&group, &state)).into_response()
}

/// DELETE /api/admin/groups/:name?force=true
///
/// 默认拒绝删除仍被引用的分组；带 `force=true` 时级联清理所有引用并删除。
pub async fn delete_group(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    Query(query): Query<super::types::DeleteGroupQuery>,
) -> impl IntoResponse {
    if !state.groups.exists(&name) {
        return (
            StatusCode::NOT_FOUND,
            Json(super::types::AdminErrorResponse::not_found(format!(
                "分组 {} 不存在",
                name
            ))),
        )
            .into_response();
    }

    let cred_count = state
        .service
        .token_manager()
        .count_credentials_with_group(&name);
    let key_count = state.client_keys.count_with_group(&name);

    if (cred_count > 0 || key_count > 0) && !query.force {
        return (
            StatusCode::CONFLICT,
            Json(super::types::AdminErrorResponse::invalid_request(format!(
                "分组仍被引用（凭据 {} / 客户端 Key {}），传 ?force=true 级联清理",
                cred_count, key_count
            ))),
        )
            .into_response();
    }

    if query.force {
        if let Err(e) = state.service.token_manager().remove_credential_group(&name) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(super::types::AdminErrorResponse::internal_error(format!(
                    "级联清理凭据失败: {}",
                    e
                ))),
            )
                .into_response();
        }
        state.client_keys.clear_group(&name);
    }

    state.groups.delete(&name);
    Json(super::types::SuccessResponse::new(format!(
        "分组 {} 已删除",
        name
    )))
    .into_response()
}
