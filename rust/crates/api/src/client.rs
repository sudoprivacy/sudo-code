use crate::error::ApiError;
use crate::prompt_cache::{PromptCache, PromptCacheRecord, PromptCacheStats};
use crate::providers::anthropic::{self, AnthropicClient, AuthSource};
use crate::providers::codex::CodexClient;
use crate::providers::openai_compat::{self, OpenAiCompatClient, OpenAiCompatConfig};
use crate::providers::registry::{ApiFormat, Credential, ResolvedProvider};
use crate::providers::{self, AuthMode, ProviderKind};
use crate::types::{MessageRequest, MessageResponse, StreamEvent};

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ProviderClient {
    Anthropic(AnthropicClient),
    Xai(OpenAiCompatClient),
    OpenAi(OpenAiCompatClient),
    Codex(CodexClient),
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
            ProviderKind::Codex => Ok(Self::Codex(CodexClient::from_auth_file()?)),
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
            AuthMode::Subscription => {
                let resolved_model = providers::resolve_model_alias(model);
                if providers::detect_provider_kind(&resolved_model) == ProviderKind::Codex {
                    return Ok(Self::Codex(CodexClient::from_auth_file()?));
                }
                Ok(Self::Anthropic(AnthropicClient::from_auth_with_mode(
                    auth,
                    Some(mode),
                )))
            }
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
                    ProviderKind::Codex => Ok(Self::Codex(CodexClient::from_auth_file()?)),
                }
            }
        }
    }

    /// Build a `ProviderClient` from a fully resolved provider config.
    ///
    /// This is the primary entry point for config-driven provider construction.
    /// The caller is responsible for calling `resolve_provider_from_config()`
    /// first to obtain the `ResolvedProvider`.
    pub fn from_resolved(
        resolved: &ResolvedProvider,
        mode: Option<AuthMode>,
    ) -> Result<Self, ApiError> {
        match resolved.api_format {
            ApiFormat::AnthropicMessages => {
                let auth = match &resolved.credential {
                    Credential::ApiKey(key) => AuthSource::ApiKey(key.clone()),
                    Credential::Token(token) => AuthSource::BearerToken(token.clone()),
                    Credential::AuthFile(path) => {
                        let content = std::fs::read_to_string(path).map_err(|e| {
                            ApiError::Configuration(format!(
                                "failed to read auth file {}: {e}",
                                path.display()
                            ))
                        })?;
                        let token = serde_json::from_str::<serde_json::Value>(&content)
                            .ok()
                            .and_then(|v| {
                                v.get("accessToken")
                                    .or_else(|| v.get("token"))
                                    .and_then(|t| t.as_str().map(String::from))
                            })
                            .unwrap_or_else(|| content.trim().to_string());
                        AuthSource::BearerToken(token)
                    }
                    Credential::None => {
                        return Err(ApiError::Configuration(
                            "no credential available for Anthropic provider".to_string(),
                        ));
                    }
                };
                let client = AnthropicClient::from_auth_with_mode(auth, mode)
                    .with_base_url(resolved.base_url.clone());
                Ok(Self::Anthropic(client))
            }
            ApiFormat::OpenAiCompletions | ApiFormat::OpenAiResponses => {
                // Build OpenAiCompatClient with the resolved credential + base URL.
                let api_key = match &resolved.credential {
                    Credential::ApiKey(key) => key.clone(),
                    Credential::Token(token) => token.clone(),
                    Credential::None => String::new(),
                    Credential::AuthFile(_) => {
                        return Err(ApiError::Configuration(
                            "auth file credential not supported for OpenAI-compat providers"
                                .to_string(),
                        ));
                    }
                };
                let config = OpenAiCompatConfig::openai();
                let client = OpenAiCompatClient::new(api_key, config)
                    .with_base_url(resolved.base_url.clone());
                match resolved.kind {
                    ProviderKind::Xai => Ok(Self::Xai(client)),
                    _ => Ok(Self::OpenAi(client)),
                }
            }
        }
    }

    #[must_use]
    pub const fn provider_kind(&self) -> ProviderKind {
        match self {
            Self::Anthropic(_) => ProviderKind::Anthropic,
            Self::Xai(_) => ProviderKind::Xai,
            Self::OpenAi(_) => ProviderKind::OpenAi,
            Self::Codex(_) => ProviderKind::Codex,
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
            Self::Xai(_) | Self::OpenAi(_) | Self::Codex(_) => None,
        }
    }

    #[must_use]
    pub fn take_last_prompt_cache_record(&self) -> Option<PromptCacheRecord> {
        match self {
            Self::Anthropic(client) => client.take_last_prompt_cache_record(),
            Self::Xai(_) | Self::OpenAi(_) | Self::Codex(_) => None,
        }
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        match self {
            Self::Anthropic(client) => client.send_message(request).await,
            Self::Xai(client) | Self::OpenAi(client) => client.send_message(request).await,
            Self::Codex(client) => client.send_message(request).await,
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
            Self::Codex(client) => client
                .stream_message(request)
                .await
                .map(MessageStream::Codex),
        }
    }
}

#[derive(Debug)]
pub enum MessageStream {
    Anthropic(anthropic::MessageStream),
    OpenAiCompat(openai_compat::MessageStream),
    Codex(crate::providers::codex::MessageStream),
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Anthropic(stream) => stream.request_id(),
            Self::OpenAiCompat(stream) => stream.request_id(),
            Self::Codex(stream) => stream.request_id(),
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        match self {
            Self::Anthropic(stream) => stream.next_event().await,
            Self::OpenAiCompat(stream) => stream.next_event().await,
            Self::Codex(stream) => stream.next_event().await,
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
