use crate::error::ApiError;
use crate::prompt_cache::{PromptCache, PromptCacheRecord, PromptCacheStats};
use crate::providers::anthropic::{self, AnthropicClient, AuthSource};
use crate::providers::openai_compat::{self, OpenAiCompatClient, OpenAiCompatConfig};
use crate::providers::registry::{ProviderProtocol, ProviderRegistry};
use crate::providers::{self, AuthMode, ProviderKind};
use crate::types::{MessageRequest, MessageResponse, StreamEvent};

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ProviderClient {
    Anthropic(AnthropicClient),
    Xai(OpenAiCompatClient),
    OpenAi(OpenAiCompatClient),
}

impl ProviderClient {
    pub fn from_model(model: &str) -> Result<Self, ApiError> {
        Self::from_model_with_anthropic_auth(model, None)
    }

    pub fn from_model_with_anthropic_auth(
        model: &str,
        anthropic_auth: Option<AuthSource>,
    ) -> Result<Self, ApiError> {
        let resolved_model = providers::resolve_model_alias(model);
        match providers::detect_provider_kind(&resolved_model) {
            ProviderKind::Anthropic => Ok(Self::Anthropic(match anthropic_auth {
                Some(auth) => AnthropicClient::from_auth(auth),
                None => AnthropicClient::from_env()?,
            })),
            ProviderKind::Xai => Ok(Self::Xai(OpenAiCompatClient::from_env(
                OpenAiCompatConfig::xai(),
            )?)),
            ProviderKind::OpenAi => {
                // DashScope models (qwen-*) also return ProviderKind::OpenAi because they
                // speak the OpenAI wire format, but they need the DashScope config which
                // reads DASHSCOPE_API_KEY and points at dashscope.aliyuncs.com.
                let config = match providers::metadata_for_model(&resolved_model) {
                    Some(meta) if meta.auth_env == "DASHSCOPE_API_KEY" => {
                        OpenAiCompatConfig::dashscope()
                    }
                    _ => OpenAiCompatConfig::openai(),
                };
                Ok(Self::OpenAi(OpenAiCompatClient::from_env(config)?))
            }
        }
    }

    /// Build a `ProviderClient` using the explicit auth mode. Proxy mode
    /// always routes through `AnthropicClient` with bearer token +
    /// `PROXY_BASE_URL`. Subscription mode uses `AnthropicClient` with
    /// OAuth token + default URL. Api-key mode routes by
    /// `detect_provider_kind()`.
    pub fn from_model_and_mode(
        model: &str,
        mode: AuthMode,
        auth: AuthSource,
    ) -> Result<Self, ApiError> {
        match mode {
            AuthMode::Proxy => {
                let base_url = anthropic::base_url_for_mode(mode);
                Ok(Self::Anthropic(
                    AnthropicClient::from_auth_with_mode(auth, Some(mode)).with_base_url(base_url),
                ))
            }
            AuthMode::Subscription => Ok(Self::Anthropic(AnthropicClient::from_auth_with_mode(
                auth,
                Some(mode),
            ))),
            AuthMode::ApiKey => {
                let resolved_model = providers::resolve_model_alias(model);
                match providers::detect_provider_kind(&resolved_model) {
                    ProviderKind::Anthropic => Ok(Self::Anthropic(
                        AnthropicClient::from_auth_with_mode(auth, Some(mode))
                            .with_base_url(anthropic::read_base_url()),
                    )),
                    ProviderKind::Xai => Ok(Self::Xai(OpenAiCompatClient::from_env(
                        OpenAiCompatConfig::xai(),
                    )?)),
                    ProviderKind::OpenAi => {
                        let config = match providers::metadata_for_model(&resolved_model) {
                            Some(meta) if meta.auth_env == "DASHSCOPE_API_KEY" => {
                                OpenAiCompatConfig::dashscope()
                            }
                            _ => OpenAiCompatConfig::openai(),
                        };
                        Ok(Self::OpenAi(OpenAiCompatClient::from_env(config)?))
                    }
                }
            }
        }
    }

    /// Build a `ProviderClient` using the config-driven registry.
    ///
    /// Resolution order:
    /// 1. Look up the model in the registry's config models.
    /// 2. If found, resolve the provider from config and build accordingly.
    /// 3. If not found, fall back to the standard `from_model_and_mode` /
    ///    `from_model` path.
    pub fn from_model_with_registry(
        model: &str,
        registry: &ProviderRegistry,
        mode: Option<AuthMode>,
        auth: Option<AuthSource>,
    ) -> Result<Self, ApiError> {
        let resolved_alias = registry.resolve_model_alias(model);

        // Try config-driven resolution first.
        if let Some(config_model) = registry.config_model(model) {
            if let Some(provider) = registry.config_provider(&config_model.provider) {
                return Self::from_config_provider(&config_model.model_id, provider, mode, auth);
            }
        }

        // Fall back to standard path.
        match (mode, auth) {
            (Some(m), Some(a)) => Self::from_model_and_mode(&resolved_alias, m, a),
            (_, Some(a)) => Self::from_model_with_anthropic_auth(&resolved_alias, Some(a)),
            _ => Self::from_model(&resolved_alias),
        }
    }

    /// Build a `ProviderClient` from an explicit config provider entry.
    fn from_config_provider(
        _model_id: &str,
        provider: &crate::providers::registry::ConfigProvider,
        mode: Option<AuthMode>,
        auth: Option<AuthSource>,
    ) -> Result<Self, ApiError> {
        match provider.protocol {
            ProviderProtocol::Anthropic => {
                // Anthropic protocol: use AnthropicClient.
                let client = match (mode, auth) {
                    (Some(m), Some(a)) => {
                        let base_url = provider
                            .base_url
                            .clone()
                            .unwrap_or_else(|| anthropic::base_url_for_mode(m));
                        AnthropicClient::from_auth_with_mode(a, Some(m)).with_base_url(base_url)
                    }
                    (_, Some(a)) => {
                        let mut client = AnthropicClient::from_auth(a);
                        if let Some(url) = &provider.base_url {
                            client = client.with_base_url(url.clone());
                        } else {
                            client = client.with_base_url(anthropic::read_base_url());
                        }
                        client
                    }
                    _ => {
                        let mut client = AnthropicClient::from_env()?;
                        if let Some(url) = &provider.base_url {
                            client = client.with_base_url(url.clone());
                        }
                        client
                    }
                };
                Ok(Self::Anthropic(client))
            }
            ProviderProtocol::OpenAiCompat => {
                // OpenAI-compat protocol: build OpenAiCompatConfig from the
                // config provider entry.
                let config = ProviderRegistry::openai_compat_config_for(provider)
                    .unwrap_or_else(OpenAiCompatConfig::openai);
                let mut client = OpenAiCompatClient::from_env(config)?;
                // If the provider specifies a fixed base_url that differs
                // from what from_env resolved (which reads the env var),
                // override it.  The env var takes precedence only when set.
                if let Some(url) = &provider.base_url {
                    let env_key = provider.base_url_env.as_deref().unwrap_or("");
                    let env_set = !env_key.is_empty()
                        && std::env::var(env_key)
                            .ok()
                            .is_some_and(|v| !v.trim().is_empty());
                    if !env_set {
                        client = client.with_base_url(url.clone());
                    }
                }
                Ok(Self::OpenAi(client))
            }
        }
    }

    #[must_use]
    pub const fn provider_kind(&self) -> ProviderKind {
        match self {
            Self::Anthropic(_) => ProviderKind::Anthropic,
            Self::Xai(_) => ProviderKind::Xai,
            Self::OpenAi(_) => ProviderKind::OpenAi,
        }
    }

    #[must_use]
    pub fn with_prompt_cache(self, prompt_cache: PromptCache) -> Self {
        match self {
            Self::Anthropic(client) => Self::Anthropic(client.with_prompt_cache(prompt_cache)),
            other => other,
        }
    }

    #[must_use]
    pub fn prompt_cache_stats(&self) -> Option<PromptCacheStats> {
        match self {
            Self::Anthropic(client) => client.prompt_cache_stats(),
            Self::Xai(_) | Self::OpenAi(_) => None,
        }
    }

    #[must_use]
    pub fn take_last_prompt_cache_record(&self) -> Option<PromptCacheRecord> {
        match self {
            Self::Anthropic(client) => client.take_last_prompt_cache_record(),
            Self::Xai(_) | Self::OpenAi(_) => None,
        }
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        match self {
            Self::Anthropic(client) => client.send_message(request).await,
            Self::Xai(client) | Self::OpenAi(client) => client.send_message(request).await,
        }
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        match self {
            Self::Anthropic(client) => client
                .stream_message(request)
                .await
                .map(MessageStream::Anthropic),
            Self::Xai(client) | Self::OpenAi(client) => client
                .stream_message(request)
                .await
                .map(MessageStream::OpenAiCompat),
        }
    }
}

#[derive(Debug)]
pub enum MessageStream {
    Anthropic(anthropic::MessageStream),
    OpenAiCompat(openai_compat::MessageStream),
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Anthropic(stream) => stream.request_id(),
            Self::OpenAiCompat(stream) => stream.request_id(),
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        match self {
            Self::Anthropic(stream) => stream.next_event().await,
            Self::OpenAiCompat(stream) => stream.next_event().await,
        }
    }
}

pub use anthropic::{
    base_url_for_mode, oauth_token_is_expired, resolve_saved_oauth_token,
    resolve_startup_auth_source, OAuthTokenSet,
};
#[must_use]
pub fn read_base_url() -> String {
    anthropic::read_base_url()
}

#[must_use]
pub fn read_xai_base_url() -> String {
    openai_compat::read_base_url(OpenAiCompatConfig::xai())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::ProviderClient;
    use crate::providers::{detect_provider_kind, resolve_model_alias, ProviderKind};

    /// Serializes every test in this module that mutates process-wide
    /// environment variables so concurrent test threads cannot observe
    /// each other's partially-applied state.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn resolves_existing_and_grok_aliases() {
        assert_eq!(resolve_model_alias("opus"), "claude-opus-4-6");
        assert_eq!(resolve_model_alias("grok"), "grok-3");
        assert_eq!(resolve_model_alias("grok-mini"), "grok-3-mini");
    }

    #[test]
    fn provider_detection_prefers_model_family() {
        assert_eq!(detect_provider_kind("grok-3"), ProviderKind::Xai);
        assert_eq!(
            detect_provider_kind("claude-sonnet-4-6"),
            ProviderKind::Anthropic
        );
    }

    /// Snapshot-restore guard for a single environment variable. Mirrors
    /// the pattern used in `providers/mod.rs` tests: captures the original
    /// value on construction, applies the override, and restores on drop so
    /// tests leave the process env untouched even when they panic.
    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn dashscope_model_uses_dashscope_config_not_openai() {
        // Regression: qwen-plus was being routed to OpenAiCompatConfig::openai()
        // which reads OPENAI_API_KEY and points at api.openai.com, when it should
        // use OpenAiCompatConfig::dashscope() which reads DASHSCOPE_API_KEY and
        // points at dashscope.aliyuncs.com.
        let _lock = env_lock();
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", Some("test-dashscope-key"));
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", None);

        let client = ProviderClient::from_model("qwen-plus");

        // Must succeed (not fail with "missing OPENAI_API_KEY")
        assert!(
            client.is_ok(),
            "qwen-plus with DASHSCOPE_API_KEY set should build successfully, got: {:?}",
            client.err()
        );

        // Verify it's the OpenAi variant pointed at the DashScope base URL.
        match client.unwrap() {
            ProviderClient::OpenAi(openai_client) => {
                assert!(
                    openai_client.base_url().contains("dashscope.aliyuncs.com"),
                    "qwen-plus should route to DashScope base URL (contains 'dashscope.aliyuncs.com'), got: {}",
                    openai_client.base_url()
                );
            }
            other => panic!("Expected ProviderClient::OpenAi for qwen-plus, got: {other:?}"),
        }
    }
}
