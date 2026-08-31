//! Admin API 中间件

use std::sync::Arc;

use parking_lot::RwLock;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};

use super::client_keys::SharedClientKeyManager;
use super::groups::SharedGroupManager;
use super::service::AdminService;
use super::types::AdminErrorResponse;
use super::usage_stats::SharedAggregator;
use super::trace_db::SharedTraceStore;
use crate::common::auth;

/// Admin API 共享状态
#[derive(Clone)]
pub struct AdminState {
    /// 登录API密钥（管理面板登录用，运行时可修改）
    pub admin_api_key: Arc<RwLock<String>>,
    /// 只读观察者密钥。`None` / 空串 = 未启用只读访问。
    pub viewer_api_key: Arc<RwLock<Option<String>>>,
    /// Admin 服务
    pub service: Arc<AdminService>,
    /// 客户端 Key 管理器（与 anthropic 路由共享）
    pub client_keys: SharedClientKeyManager,
    /// 用量聚合器（与 anthropic 路由共享）
    pub usage_aggregator: SharedAggregator,
    /// 请求链路追踪存储（与 anthropic 路由共享）
    pub trace_store: SharedTraceStore,
    /// 原始请求体留存（与 anthropic 路由共享；未开启时为 None）
    pub prompt_store: Option<Arc<crate::admin::prompt_store::PromptStore>>,
    /// 账号分组注册表（持久化到 groups.json）
    pub groups: SharedGroupManager,
    /// 分钟级速率环（与 anthropic 路由共享），RPM/TPM 的唯一数据源。
    ///
    /// `Option` 是为了兼容不注入它的嵌入式/测试装配；生产路径恒为 `Some`。
    pub rate_ring: Option<crate::anthropic::rate_ring::SharedRateRing>,
    /// 计价表（credit→USD 汇率 + 模型官方牌价），启动时从配置解析，只读。
    pub pricing: Arc<crate::common::pricing::PricingTable>,
}

impl AdminState {
    pub fn new(
        admin_api_key: impl Into<String>,
        service: AdminService,
        client_keys: SharedClientKeyManager,
        usage_aggregator: SharedAggregator,
        trace_store: SharedTraceStore,
        groups: SharedGroupManager,
    ) -> Self {
        Self {
            admin_api_key: Arc::new(RwLock::new(admin_api_key.into())),
            viewer_api_key: Arc::new(RwLock::new(None)),
            service: Arc::new(service),
            client_keys,
            usage_aggregator,
            trace_store,
            groups,
            rate_ring: None,
            prompt_store: None,
            pricing: Arc::new(crate::common::pricing::PricingTable::default()),
        }
    }

    /// 注入只读观察者密钥。空串按未配置处理。
    ///
    /// 与 admin key 相同时只警告不拒绝启动：拒绝启动会让一次配置笔误变成停服，
    /// 比"权限没降下来"更糟。中间件那边也会把它判成 Admin，行为一致。
    pub fn with_viewer_api_key(self, key: Option<String>) -> Self {
        let key = key.filter(|k| !k.trim().is_empty());
        if let Some(ref k) = key {
            let admin = self.admin_api_key.read().clone();
            if *k == admin {
                tracing::warn!(
                    "viewerApiKey 与 adminApiKey 相同 —— 只读密钥没有起到降权作用，请改成不同的值"
                );
            }
        }
        *self.viewer_api_key.write() = key;
        self
    }

    /// 注入原始请求体留存（与 anthropic 路由共享同一个实例）。
    pub fn with_prompt_store(
        mut self,
        store: Option<Arc<crate::admin::prompt_store::PromptStore>>,
    ) -> Self {
        self.prompt_store = store;
        self
    }

    /// 注入按配置解析的计价表（缺省时用内置默认表）。
    pub fn with_pricing(mut self, pricing: crate::common::pricing::PricingTable) -> Self {
        self.pricing = Arc::new(pricing);
        self
    }

    /// 注入分钟级速率环（RPM/TPM 数据源）。
    pub fn with_rate_ring(
        mut self,
        ring: Option<crate::anthropic::rate_ring::SharedRateRing>,
    ) -> Self {
        self.rate_ring = ring;
        self
    }
}

/// 只读观察者可以访问的路径前缀。
///
/// 白名单而不是黑名单：新增一个 handler 时，默认是**不可见**的。反过来做的话
/// 每加一个接口都得记得去补黑名单，漏一次就是越权。
///
/// 这些路径返回的都是脱敏后的聚合量（见 `handlers::viewer_traffic`）。
/// 特意**不含** `/credentials`、`/client-keys`、`/traces`、`/config`、`/stats/*` ——
/// 它们各自带客户 Key 名、成本毛利、凭据明细或请求体。
const VIEWER_ALLOWED: &[&str] = &["/viewer/traffic", "/viewer/session"];

/// 谁在调这个接口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminRole {
    /// 全权。
    Admin,
    /// 只读观察者：仅 GET，且仅 `VIEWER_ALLOWED` 里的路径。
    Viewer,
}

fn viewer_may_access(path: &str) -> bool {
    // 含 `..` 的一律拒。这个检查跑在**路由之前**，所以"我放行的前缀"和"最终
    // 落到的 handler"可能不是同一个东西：`/viewer/traffic/../credentials` 的
    // 前缀是白名单里的，归一化之后却指向 credentials。不去猜上游会不会先归一化，
    // 直接拒掉带 `..` 的路径 —— 正常请求里没有它。
    if path.contains("..") {
        return false;
    }
    // 去掉 axum nest 前缀后再比。请求进到这里时 path 是 `/api/admin/...`。
    let tail = path.strip_prefix("/api/admin").unwrap_or(path);
    VIEWER_ALLOWED
        .iter()
        .any(|p| tail == *p || tail.starts_with(&format!("{p}/")))
}

/// Admin API 认证中间件 — 校验登录API密钥（adminApiKey）或只读密钥（viewerApiKey）
pub async fn admin_auth_middleware(
    State(state): State<AdminState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(key) = auth::extract_api_key(&request) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(AdminErrorResponse::authentication_error()),
        )
            .into_response();
    };

    let admin_key = state.admin_api_key.read().clone();
    // 先比 admin：两把 key 若被配成同一个值，按高权限处理，不给「看起来降权了」的假象。
    let role = if auth::constant_time_eq(&key, &admin_key) {
        AdminRole::Admin
    } else {
        let viewer_key = state.viewer_api_key.read().clone();
        match viewer_key {
            Some(vk) if !vk.is_empty() && auth::constant_time_eq(&key, &vk) => AdminRole::Viewer,
            _ => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(AdminErrorResponse::authentication_error()),
                )
                    .into_response();
            }
        }
    };

    if role == AdminRole::Viewer {
        let path = request.uri().path().to_string();
        let is_read = request.method() == axum::http::Method::GET;
        if !is_read || !viewer_may_access(&path) {
            // 403 而不是 404：这个 key 是有效的，只是不该碰这里。
            // 说清楚它能干什么，免得对方以为 key 发错了。
            return (
                StatusCode::FORBIDDEN,
                Json(AdminErrorResponse::forbidden(
                    "只读密钥仅可 GET /api/admin/viewer/*（流量概览）",
                )),
            )
                .into_response();
        }
    }

    let mut request = request;
    request.extensions_mut().insert(role);
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_whitelist_is_exact_and_prefix_safe() {
        assert!(viewer_may_access("/api/admin/viewer/traffic"));
        assert!(viewer_may_access("/api/admin/viewer/session"));
        assert!(viewer_may_access("/api/admin/viewer/traffic/hourly"));
    }

    #[test]
    fn viewer_cannot_reach_anything_sensitive() {
        for p in [
            "/api/admin/credentials",
            "/api/admin/credentials/export",
            "/api/admin/credentials/7",
            "/api/admin/client-keys",
            "/api/admin/traces",
            "/api/admin/traces/abc/prompt",
            "/api/admin/stats/overview",
            "/api/admin/stats/by-credential",
            "/api/admin/config",
            "/api/admin/groups",
        ] {
            assert!(!viewer_may_access(p), "{p} 不该对只读密钥可见");
        }
    }

    #[test]
    fn a_path_that_merely_starts_with_viewer_is_not_allowed() {
        // 防止 `/viewer-secrets` 这种靠前缀蹭进来。
        assert!(!viewer_may_access("/api/admin/viewerx"));
        assert!(!viewer_may_access("/api/admin/viewer-secrets"));
    }

    #[test]
    fn dot_dot_traversal_is_rejected_even_under_an_allowed_prefix() {
        // 这个检查在路由之前跑，所以放行的前缀与最终 handler 可能不一致。
        // 首次写这条时它是红的 —— 前缀匹配确实放行了下面这些。
        assert!(!viewer_may_access("/api/admin/viewer/traffic/../credentials"));
        assert!(!viewer_may_access("/api/admin/viewer/session/../../admin/config"));
        assert!(!viewer_may_access("/api/admin/viewer/traffic/.."));
    }
}
