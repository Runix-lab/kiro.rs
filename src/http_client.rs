//! HTTP Client 构建模块
//!
//! 提供统一的 HTTP Client 构建功能，支持代理配置

use reqwest::{Client, Proxy};
use std::time::Duration;

use crate::model::config::TlsBackend;

/// 代理配置
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ProxyConfig {
    /// 代理地址，支持 http/https/socks5
    pub url: String,
    /// 代理认证用户名
    pub username: Option<String>,
    /// 代理认证密码
    pub password: Option<String>,
}

impl ProxyConfig {
    /// 从 url 创建代理配置
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            username: None,
            password: None,
        }
    }

    /// 设置认证信息
    pub fn with_auth(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }
}

/// 构建 HTTP Client
///
/// # Arguments
/// * `proxy` - 可选的代理配置
/// * `timeout_secs` - 超时时间（秒）
///
/// # Returns
/// 配置好的 reqwest::Client
pub fn build_client(
    proxy: Option<&ProxyConfig>,
    timeout_secs: u64,
    tls_backend: TlsBackend,
) -> anyhow::Result<Client> {
    build_client_inner(proxy, Timeout::Whole(timeout_secs), tls_backend)
}

/// 流式上游调用专用：用**读空闲**超时替代整请求超时。
///
/// `Client::builder().timeout()` 是整个请求的墙钟上限，且一直作用到 body 读取结束——
/// 对流式接口意味着我们会在第 N 秒亲手掐断一条正在正常产出的流。实测：三条已推送
/// 690-843 KB、1.2 万 output token 的 opus-5 请求在 720.04 秒被切成 `interrupted`，
/// 而终止性的 `meteringEvent` 恰好在流末尾，于是这些请求全部记 0 credit。
///
/// 改用 `read_timeout`（两次数据之间的最大间隔）后语义与 nginx 的 `proxy_read_timeout`
/// 一致：只要上游还在吐数据就不打断，真死掉的连接仍会被回收。
pub fn build_streaming_client(
    proxy: Option<&ProxyConfig>,
    idle_timeout_secs: u64,
    tls_backend: TlsBackend,
) -> anyhow::Result<Client> {
    build_client_inner(proxy, Timeout::Idle(idle_timeout_secs), tls_backend)
}

enum Timeout {
    /// 整请求墙钟上限（含 body）
    Whole(u64),
    /// 两次读取之间的最大间隔
    Idle(u64),
}

fn build_client_inner(
    proxy: Option<&ProxyConfig>,
    timeout: Timeout,
    tls_backend: TlsBackend,
) -> anyhow::Result<Client> {
    let mut builder = match timeout {
        Timeout::Whole(secs) => Client::builder().timeout(Duration::from_secs(secs)),
        Timeout::Idle(secs) => Client::builder()
            .read_timeout(Duration::from_secs(secs))
            .connect_timeout(Duration::from_secs(30)),
    };

    match tls_backend {
        TlsBackend::Rustls => {
            builder = builder.use_rustls_tls();
        }
        TlsBackend::NativeTls => {
            #[cfg(feature = "native-tls")]
            {
                builder = builder.use_native_tls();
            }
            #[cfg(not(feature = "native-tls"))]
            {
                anyhow::bail!("此构建版本未包含 native-tls 后端，请在配置中改用 rustls");
            }
        }
    }

    if let Some(proxy_config) = proxy {
        let mut proxy = Proxy::all(&proxy_config.url)?;

        // 设置代理认证
        if let (Some(username), Some(password)) = (&proxy_config.username, &proxy_config.password) {
            proxy = proxy.basic_auth(username, password);
        }

        builder = builder.proxy(proxy);
        tracing::debug!("HTTP Client 使用代理: {}", proxy_config.url);
    }

    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_config_new() {
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        assert_eq!(config.url, "http://127.0.0.1:7890");
        assert!(config.username.is_none());
        assert!(config.password.is_none());
    }

    #[test]
    fn test_proxy_config_with_auth() {
        let config = ProxyConfig::new("socks5://127.0.0.1:1080").with_auth("user", "pass");
        assert_eq!(config.url, "socks5://127.0.0.1:1080");
        assert_eq!(config.username, Some("user".to_string()));
        assert_eq!(config.password, Some("pass".to_string()));
    }

    #[test]
    fn test_build_client_without_proxy() {
        let client = build_client(None, 30, TlsBackend::Rustls);
        assert!(client.is_ok());
    }

    #[test]
    fn test_build_client_with_proxy() {
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        let client = build_client(Some(&config), 30, TlsBackend::Rustls);
        assert!(client.is_ok());
    }
}
