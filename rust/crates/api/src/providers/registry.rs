//! Config-driven provider & model registry.
//!
//! Resolves model alias → auth mode → provider → connection details using the
//! `sudocode.json` config file.  Falls back to hardcoded registry when config
//! is absent.

use std::path::PathBuf;

use super::{AuthMode, ProviderKind};
use crate::error::ApiError;

// Re-export the config types from the runtime crate so consumers can use them
// via `api::providers::registry::*` without depending on runtime directly.
pub use runtime::config::{
    ModelConfigEntry, ModelProviderMapping, ProviderConnectionConfig, SudoCodeConfig,
};

// ---------------------------------------------------------------------------
// Types (API-layer only)
// ---------------------------------------------------------------------------

/// Wire protocol / API format for talking to a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiFormat {
    /// Native Anthropic Messages API (`/v1/messages`).
    AnthropicMessages,
    /// OpenAI-compatible chat completions API (`/v1/chat/completions`).
    OpenAiCompletions,
    /// OpenAI Responses API (`/v1/responses`).
    OpenAiResponses,
}

/// Resolved credential for authenticating with a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// Direct API key string.
    ApiKey(String),
    /// Bearer / OAuth token string.
    Token(String),
    /// Path to a credentials file (e.g. `~/.claude/credentials.json`).
    AuthFile(PathBuf),
    /// No credential available — provider may not require one.
    None,
}

/// Fully resolved provider information — everything needed to build a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProvider {
    pub kind: ProviderKind,
    pub api_format: ApiFormat,
    pub base_url: String,
    pub credential: Credential,
    /// The wire model ID to send to the provider.
    pub model_id: String,
    /// Human-friendly display name for UI banners.
    pub display_name: Option<String>,
}

// ---------------------------------------------------------------------------
// SudoCodeConfig helpers
// ---------------------------------------------------------------------------

/// Look up a model by alias (case-insensitive).
#[must_use]
pub fn resolve_model<'a>(config: &'a SudoCodeConfig, alias: &str) -> Option<&'a ModelConfigEntry> {
    config.models.get(&alias.trim().to_ascii_lowercase())
}

/// List available auth modes for a model alias, in config order.
#[must_use]
pub fn available_auth_modes<'a>(config: &'a SudoCodeConfig, alias: &str) -> Vec<&'a str> {
    resolve_model(config, alias)
        .map(|m| m.providers.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

/// Look up connection config for a provider under a given auth mode.
#[must_use]
pub fn connection_for<'a>(
    config: &'a SudoCodeConfig,
    auth_mode: &str,
    provider_name: &str,
) -> Option<&'a ProviderConnectionConfig> {
    config.auth_modes.get(auth_mode)?.get(provider_name)
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Resolve a model alias + optional auth mode through the config into a fully
/// resolved provider specification.
///
/// Resolution flow:
/// 1. Look up `models.<alias>` → get available auth modes
/// 2. If `explicit_auth` specified, use it; otherwise pick first available
/// 3. From `models.<alias>.providers.<mode>` get `{ provider, model, api? }`
/// 4. From `auth_modes.<mode>.<provider>` get connection details
/// 5. Resolve wire format: non-proxy → infer from provider type; proxy → use `api` field
/// 6. Resolve credentials from connection config
pub fn resolve_provider_from_config(
    model_alias: &str,
    explicit_auth: Option<AuthMode>,
    config: &SudoCodeConfig,
) -> Result<ResolvedProvider, ApiError> {
    let alias_lower = model_alias.trim().to_ascii_lowercase();

    // 1. Look up the model in config.
    let model_config = resolve_model(config, &alias_lower).ok_or_else(|| {
        ApiError::Configuration(format!(
            "model alias '{model_alias}' not found in sudocode.json"
        ))
    })?;

    // 2. Determine the auth mode to use.
    let auth_mode_str = match explicit_auth {
        Some(mode) => {
            let s = mode.as_str();
            if !model_config.providers.contains_key(s) {
                return Err(ApiError::Configuration(format!(
                    "auth mode '{}' is not available for model '{}'. Available: {}",
                    s,
                    model_alias,
                    model_config
                        .providers
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            s.to_string()
        }
        None => {
            // Pick first available (BTreeMap is ordered, config order = priority).
            model_config
                .providers
                .keys()
                .next()
                .cloned()
                .ok_or_else(|| {
                    ApiError::Configuration(format!(
                        "model '{}' has no provider mappings in sudocode.json",
                        model_alias
                    ))
                })?
        }
    };

    // 3. Get the provider mapping for this auth mode.
    let mapping = model_config.providers.get(&auth_mode_str).ok_or_else(|| {
        ApiError::Configuration(format!(
            "no provider mapping for auth mode '{auth_mode_str}' on model '{model_alias}'"
        ))
    })?;

    // 4. Get connection config.
    let connection =
        connection_for(config, &auth_mode_str, &mapping.provider).ok_or_else(|| {
            ApiError::Configuration(format!(
                "provider '{}' not found under auth_modes.{} in sudocode.json",
                mapping.provider, auth_mode_str
            ))
        })?;

    // 5. Determine API format.
    let api_format = resolve_api_format(&auth_mode_str, &mapping.provider, mapping.api.as_deref())?;

    // 6. Resolve credential.
    let credential = resolve_credential(&auth_mode_str, connection)?;

    // 7. Determine provider kind from the provider name / api format.
    let kind = infer_provider_kind(&mapping.provider, api_format);

    Ok(ResolvedProvider {
        kind,
        api_format,
        base_url: connection.base_url.clone(),
        credential,
        model_id: mapping.model.clone(),
        display_name: Some(model_config.name.clone()),
    })
}

/// Resolve the wire API format.
///
/// - Proxy providers: must have an `api` field (`"openai-completions"` or `"openai-responses"`).
/// - Non-proxy providers: inferred from the provider name.
fn resolve_api_format(
    auth_mode: &str,
    provider_name: &str,
    api_override: Option<&str>,
) -> Result<ApiFormat, ApiError> {
    // If there's an explicit `api` field, use it.
    if let Some(api) = api_override {
        return match api {
            "openai-completions" => Ok(ApiFormat::OpenAiCompletions),
            "openai-responses" => Ok(ApiFormat::OpenAiResponses),
            "anthropic-messages" => Ok(ApiFormat::AnthropicMessages),
            other => Err(ApiError::Configuration(format!(
                "unknown api format '{other}' for provider '{provider_name}' under mode '{auth_mode}'"
            ))),
        };
    }

    // For proxy mode without an explicit `api`, default to OpenAI completions.
    if auth_mode == "proxy" {
        return Ok(ApiFormat::OpenAiCompletions);
    }

    // Infer from provider name.
    match provider_name {
        "anthropic" | "claude" => Ok(ApiFormat::AnthropicMessages),
        "openai" | "xai" | "dashscope" | "google" | "codex" | "gemini" => {
            Ok(ApiFormat::OpenAiCompletions)
        }
        // Unknown providers under api-key mode: default to OpenAI-compatible.
        _ => Ok(ApiFormat::OpenAiCompletions),
    }
}

/// Resolve credentials from the connection config.
///
/// - `api-key` mode: inline `apiKey` → `apiKeyEnv` from env
/// - `subscription` mode: inline `token` → `tokenEnv` from env → `authFile`
/// - `proxy` mode: inline `apiKey` → `apiKeyEnv` from env
fn resolve_credential(
    auth_mode: &str,
    connection: &ProviderConnectionConfig,
) -> Result<Credential, ApiError> {
    match auth_mode {
        "api-key" | "proxy" => {
            // Inline API key takes priority.
            if let Some(key) = &connection.api_key {
                if !key.is_empty() {
                    return Ok(Credential::ApiKey(key.clone()));
                }
            }
            // Then env var.
            if let Some(env_name) = &connection.api_key_env {
                if let Ok(val) = std::env::var(env_name) {
                    if !val.trim().is_empty() {
                        return Ok(Credential::ApiKey(val));
                    }
                }
            }
            // For proxy mode, allow no credential (some proxies don't need auth).
            if auth_mode == "proxy" {
                return Ok(Credential::None);
            }
            Err(ApiError::Configuration(format!(
                "no API key available for provider under auth mode '{auth_mode}'. \
                 Set apiKey or apiKeyEnv in sudocode.json, or set the appropriate env var."
            )))
        }
        "subscription" => {
            // 1. Inline token.
            if let Some(token) = &connection.token {
                if !token.is_empty() {
                    return Ok(Credential::Token(token.clone()));
                }
            }
            // 2. Token env var.
            if let Some(env_name) = &connection.token_env {
                if let Ok(val) = std::env::var(env_name) {
                    if !val.trim().is_empty() {
                        return Ok(Credential::Token(val));
                    }
                }
            }
            // 3. Auth file.
            if let Some(path) = &connection.auth_file {
                let expanded = expand_tilde(path);
                if expanded.exists() {
                    return Ok(Credential::AuthFile(expanded));
                }
            }
            Err(ApiError::Configuration(format!(
                "no token available for subscription provider. \
                 Set token, tokenEnv, or authFile in sudocode.json."
            )))
        }
        _ => {
            // Unknown auth mode — try apiKey then token.
            if let Some(key) = &connection.api_key {
                if !key.is_empty() {
                    return Ok(Credential::ApiKey(key.clone()));
                }
            }
            if let Some(token) = &connection.token {
                if !token.is_empty() {
                    return Ok(Credential::Token(token.clone()));
                }
            }
            Ok(Credential::None)
        }
    }
}

/// Infer `ProviderKind` from the provider name and API format.
fn infer_provider_kind(provider_name: &str, api_format: ApiFormat) -> ProviderKind {
    match api_format {
        ApiFormat::AnthropicMessages => ProviderKind::Anthropic,
        ApiFormat::OpenAiCompletions | ApiFormat::OpenAiResponses => match provider_name {
            "xai" => ProviderKind::Xai,
            _ => ProviderKind::OpenAi,
        },
    }
}

/// Expand `~` prefix to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn sample_config() -> SudoCodeConfig {
        let mut auth_modes = BTreeMap::new();

        // subscription mode
        let mut subscription = BTreeMap::new();
        subscription.insert(
            "claude".to_string(),
            ProviderConnectionConfig {
                base_url: "https://api.anthropic.com/v1/messages".to_string(),
                api_key: None,
                api_key_env: None,
                token: None,
                token_env: Some("CLAUDE_CODE_OAUTH_TOKEN".to_string()),
                auth_file: Some("~/.claude/credentials.json".to_string()),
            },
        );
        auth_modes.insert("subscription".to_string(), subscription);

        // proxy mode
        let mut proxy = BTreeMap::new();
        proxy.insert(
            "sudorouter".to_string(),
            ProviderConnectionConfig {
                base_url: "https://hk.sudorouter.ai/v1".to_string(),
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                token: None,
                token_env: None,
                auth_file: None,
            },
        );
        auth_modes.insert("proxy".to_string(), proxy);

        // api-key mode
        let mut api_key = BTreeMap::new();
        api_key.insert(
            "anthropic".to_string(),
            ProviderConnectionConfig {
                base_url: "https://api.anthropic.com".to_string(),
                api_key: None,
                api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                token: None,
                token_env: None,
                auth_file: None,
            },
        );
        api_key.insert(
            "xai".to_string(),
            ProviderConnectionConfig {
                base_url: "https://api.x.ai/v1".to_string(),
                api_key: None,
                api_key_env: Some("XAI_API_KEY".to_string()),
                token: None,
                token_env: None,
                auth_file: None,
            },
        );
        auth_modes.insert("api-key".to_string(), api_key);

        // models
        let mut models = BTreeMap::new();
        let mut opus_providers = BTreeMap::new();
        opus_providers.insert(
            "subscription".to_string(),
            ModelProviderMapping {
                provider: "claude".to_string(),
                model: "claude-opus-4-6".to_string(),
                api: None,
            },
        );
        opus_providers.insert(
            "proxy".to_string(),
            ModelProviderMapping {
                provider: "sudorouter".to_string(),
                model: "claude-opus-4-6".to_string(),
                api: Some("openai-completions".to_string()),
            },
        );
        opus_providers.insert(
            "api-key".to_string(),
            ModelProviderMapping {
                provider: "anthropic".to_string(),
                model: "claude-opus-4-6".to_string(),
                api: None,
            },
        );
        models.insert(
            "opus".to_string(),
            ModelConfigEntry {
                alias: "opus".to_string(),
                name: "Claude Opus 4.6".to_string(),
                input: vec!["text".to_string()],
                providers: opus_providers,
            },
        );

        let mut grok_providers = BTreeMap::new();
        grok_providers.insert(
            "api-key".to_string(),
            ModelProviderMapping {
                provider: "xai".to_string(),
                model: "grok-3".to_string(),
                api: None,
            },
        );
        models.insert(
            "grok".to_string(),
            ModelConfigEntry {
                alias: "grok".to_string(),
                name: "Grok 3".to_string(),
                input: vec!["text".to_string()],
                providers: grok_providers,
            },
        );

        SudoCodeConfig { auth_modes, models }
    }

    #[test]
    fn resolve_model_finds_alias() {
        let config = sample_config();
        assert!(resolve_model(&config, "opus").is_some());
        assert!(resolve_model(&config, "OPUS").is_some()); // case insensitive
        assert!(resolve_model(&config, "unknown").is_none());
    }

    #[test]
    fn available_auth_modes_lists_keys() {
        let config = sample_config();
        let modes = available_auth_modes(&config, "opus");
        assert!(modes.contains(&"subscription"));
        assert!(modes.contains(&"proxy"));
        assert!(modes.contains(&"api-key"));

        let grok_modes = available_auth_modes(&config, "grok");
        assert_eq!(grok_modes, vec!["api-key"]);
    }

    #[test]
    fn connection_for_resolves() {
        let config = sample_config();
        let conn =
            connection_for(&config, "proxy", "sudorouter").expect("should find proxy sudorouter");
        assert_eq!(conn.base_url, "https://hk.sudorouter.ai/v1");
        assert_eq!(conn.api_key.as_deref(), Some("sk-test-key"));
    }

    #[test]
    fn resolve_provider_proxy_with_inline_key() {
        let config = sample_config();
        let resolved = resolve_provider_from_config("opus", Some(AuthMode::Proxy), &config)
            .expect("should resolve");
        assert_eq!(resolved.kind, ProviderKind::OpenAi);
        assert_eq!(resolved.api_format, ApiFormat::OpenAiCompletions);
        assert_eq!(resolved.base_url, "https://hk.sudorouter.ai/v1");
        assert_eq!(
            resolved.credential,
            Credential::ApiKey("sk-test-key".to_string())
        );
        assert_eq!(resolved.model_id, "claude-opus-4-6");
    }

    #[test]
    fn resolve_provider_picks_first_mode_by_default() {
        let config = sample_config();
        // First mode in BTreeMap for "grok" is "api-key" (only one).
        let resolved = resolve_provider_from_config("grok", None, &config);
        // This will fail credential resolution since XAI_API_KEY is not set in
        // test env, but the error should mention the right auth mode.
        match resolved {
            Err(ApiError::Configuration(msg)) => {
                assert!(msg.contains("API key"), "unexpected error: {msg}");
            }
            Ok(r) => {
                // If XAI_API_KEY happens to be set in the env, that's fine too.
                assert_eq!(r.kind, ProviderKind::Xai);
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn resolve_provider_rejects_unavailable_mode() {
        let config = sample_config();
        let result = resolve_provider_from_config("grok", Some(AuthMode::Subscription), &config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not available"), "unexpected error: {msg}");
    }

    #[test]
    fn resolve_api_format_infers_correctly() {
        assert_eq!(
            resolve_api_format("api-key", "anthropic", None).unwrap(),
            ApiFormat::AnthropicMessages
        );
        assert_eq!(
            resolve_api_format("api-key", "openai", None).unwrap(),
            ApiFormat::OpenAiCompletions
        );
        assert_eq!(
            resolve_api_format("proxy", "any", Some("openai-responses")).unwrap(),
            ApiFormat::OpenAiResponses
        );
        assert_eq!(
            resolve_api_format("proxy", "any", None).unwrap(),
            ApiFormat::OpenAiCompletions
        );
    }

    #[test]
    fn infer_provider_kind_maps_correctly() {
        assert_eq!(
            infer_provider_kind("anthropic", ApiFormat::AnthropicMessages),
            ProviderKind::Anthropic
        );
        assert_eq!(
            infer_provider_kind("xai", ApiFormat::OpenAiCompletions),
            ProviderKind::Xai
        );
        assert_eq!(
            infer_provider_kind("openai", ApiFormat::OpenAiCompletions),
            ProviderKind::OpenAi
        );
    }

    #[test]
    fn empty_config_is_empty() {
        let config = SudoCodeConfig::default();
        assert!(config.auth_modes.is_empty());
        assert!(config.models.is_empty());
    }

    #[test]
    fn expand_tilde_works() {
        let path = expand_tilde("~/.claude/credentials.json");
        if let Some(home) = std::env::var_os("HOME") {
            let expected = std::path::PathBuf::from(home).join(".claude/credentials.json");
            assert_eq!(path, expected);
        }
    }
}
