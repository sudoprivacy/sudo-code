//! Config-driven provider & model registry.
//!
//! Layers user-defined `"providers"` and `"models"` entries from the JSON
//! config on top of the built-in hardcoded registry so that:
//!
//! - Config entries take precedence for matching aliases.
//! - Hardcoded entries remain available when no config is present.
//! - Unknown models still fall through to prefix-based detection.

use std::collections::BTreeMap;

use super::openai_compat::{self, OpenAiCompatConfig};
use super::{ModelTokenLimit, ProviderKind, ProviderMetadata};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Wire protocol family for a configured provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderProtocol {
    /// Native Anthropic Messages API (`/v1/messages`).
    Anthropic,
    /// OpenAI-compatible chat completions API (`/v1/chat/completions`).
    OpenAiCompat,
}

/// A provider endpoint defined in the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProvider {
    /// Protocol used to talk to this provider.
    pub protocol: ProviderProtocol,
    /// Env-var name whose value is the API key (e.g. `"MY_KEY"`).
    pub api_key_env: Option<String>,
    /// Fixed base URL.  Takes precedence over `base_url_env`.
    pub base_url: Option<String>,
    /// Env-var name whose value overrides the base URL at runtime.
    pub base_url_env: Option<String>,
    /// Human-friendly label shown in the connection banner.
    pub display_name: Option<String>,
}

/// A model binding defined in the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigModel {
    /// Key into the `providers` map.
    pub provider: String,
    /// Model identifier sent over the wire (e.g. `"claude-opus-4-6"`).
    pub model_id: String,
    /// Optional max output tokens override.
    pub max_output_tokens: Option<u32>,
    /// Optional context window override.
    pub context_window: Option<u32>,
}

/// Combined config-driven + hardcoded provider/model registry.
///
/// The zero-value (`Default`) contains no config entries, making every
/// resolution fall through to the hardcoded registry.
#[derive(Debug, Clone, Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, ConfigProvider>,
    models: BTreeMap<String, ConfigModel>,
}

/// Metadata resolved through the registry for a given model.
#[derive(Debug, Clone)]
pub struct ResolvedProviderMeta {
    pub kind: ProviderKind,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub base_url_env: Option<String>,
    /// Provider display name for UI banners.  `None` means use the default
    /// label derived from `ProviderKind`.
    pub display_name: Option<String>,
}

// ---------------------------------------------------------------------------
// ProviderRegistry
// ---------------------------------------------------------------------------

impl ProviderRegistry {
    /// Build a registry from parsed config sections.
    #[must_use]
    pub fn new(
        providers: BTreeMap<String, ConfigProvider>,
        models: BTreeMap<String, ConfigModel>,
    ) -> Self {
        Self { providers, models }
    }

    /// Whether the config contributed any providers or models.
    #[must_use]
    pub fn is_config_empty(&self) -> bool {
        self.providers.is_empty() && self.models.is_empty()
    }

    #[must_use]
    pub fn providers(&self) -> &BTreeMap<String, ConfigProvider> {
        &self.providers
    }

    #[must_use]
    pub fn models(&self) -> &BTreeMap<String, ConfigModel> {
        &self.models
    }

    // -- Resolution helpers ------------------------------------------------

    /// Look up a config model entry by alias (case-insensitive).
    #[must_use]
    pub fn config_model(&self, alias: &str) -> Option<&ConfigModel> {
        let key = alias.trim().to_ascii_lowercase();
        self.models.get(&key)
    }

    /// Look up a config provider entry by name.
    #[must_use]
    pub fn config_provider(&self, name: &str) -> Option<&ConfigProvider> {
        self.providers.get(name)
    }

    /// Resolve a model alias.  Config models take precedence; falls back to
    /// the built-in hardcoded alias table.
    #[must_use]
    pub fn resolve_model_alias(&self, alias: &str) -> String {
        if let Some(entry) = self.config_model(alias) {
            return entry.model_id.clone();
        }
        super::resolve_model_alias(alias)
    }

    /// Resolve full provider metadata for a model.  Checks config first
    /// (by alias *and* by canonical model ID), then falls back to the
    /// hardcoded `metadata_for_model`.
    #[must_use]
    pub fn metadata_for_model(&self, model: &str) -> Option<ResolvedProviderMeta> {
        // 1. Direct alias match in config models.
        if let Some(meta) = self.resolve_config_meta_by_alias(model) {
            return Some(meta);
        }
        // 2. Resolve via hardcoded alias, then check if the canonical ID
        //    matches a config model entry's model_id.
        let canonical = super::resolve_model_alias(model);
        if canonical != model {
            if let Some(meta) = self.resolve_config_meta_by_model_id(&canonical) {
                return Some(meta);
            }
        }
        // 3. Check config models by model_id directly.
        if let Some(meta) = self.resolve_config_meta_by_model_id(model) {
            return Some(meta);
        }
        // 4. Fall back to hardcoded.
        super::metadata_for_model(model).map(hardcoded_to_resolved)
    }

    /// Detect provider kind for a model.
    #[must_use]
    pub fn detect_provider_kind(&self, model: &str) -> ProviderKind {
        if let Some(meta) = self.metadata_for_model(model) {
            return meta.kind;
        }
        super::detect_provider_kind(model)
    }

    /// Token limits for a model.
    #[must_use]
    pub fn model_token_limit(&self, model: &str) -> Option<ModelTokenLimit> {
        if let Some(entry) = self.config_model(model) {
            if entry.max_output_tokens.is_some() || entry.context_window.is_some() {
                return Some(ModelTokenLimit {
                    max_output_tokens: entry.max_output_tokens.unwrap_or(64_000),
                    context_window_tokens: entry.context_window.unwrap_or(200_000),
                });
            }
            // Config model without explicit limits → try hardcoded for the
            // wire model_id.
            return super::model_token_limit(&entry.model_id);
        }
        super::model_token_limit(model)
    }

    /// Max output tokens for a model.
    #[must_use]
    pub fn max_tokens_for_model(&self, model: &str) -> u32 {
        self.model_token_limit(model).map_or_else(
            || {
                let canonical = self.resolve_model_alias(model);
                if canonical.contains("opus") {
                    32_000
                } else {
                    64_000
                }
            },
            |limit| limit.max_output_tokens,
        )
    }

    /// Build an [`OpenAiCompatConfig`] from a [`ConfigProvider`].
    ///
    /// Returns `None` if the provider uses the Anthropic protocol.
    #[must_use]
    pub fn openai_compat_config_for(provider: &ConfigProvider) -> Option<OpenAiCompatConfig> {
        if provider.protocol != ProviderProtocol::OpenAiCompat {
            return None;
        }
        // We construct a config with the provider's settings.  The
        // `api_key_env` and `base_url_env` are leaked into 'static
        // references via `Box::leak` because `OpenAiCompatConfig` requires
        // `&'static str` fields.  This is acceptable because provider
        // configs are loaded once at startup and live for the process
        // lifetime.
        let api_key_env: &'static str = provider
            .api_key_env
            .as_deref()
            .map_or("OPENAI_API_KEY", |s| leak_string(s));
        let base_url_env: &'static str = provider
            .base_url_env
            .as_deref()
            .map_or("OPENAI_BASE_URL", |s| leak_string(s));
        let default_base_url: &'static str = provider
            .base_url
            .as_deref()
            .map_or(openai_compat::DEFAULT_OPENAI_BASE_URL, |s| leak_string(s));
        let provider_name: &'static str = provider
            .display_name
            .as_deref()
            .map_or("OpenAI-compat", |s| leak_string(s));

        Some(OpenAiCompatConfig {
            provider_name,
            api_key_env,
            base_url_env,
            default_base_url,
            // Use a reasonable default; custom providers rarely have tight
            // body limits.
            max_request_body_bytes: 104_857_600, // 100 MB
        })
    }

    // -- Private helpers ---------------------------------------------------

    fn resolve_config_meta_by_alias(&self, alias: &str) -> Option<ResolvedProviderMeta> {
        let model_entry = self.config_model(alias)?;
        let provider_entry = self.providers.get(&model_entry.provider)?;
        Some(config_to_resolved(model_entry, provider_entry))
    }

    fn resolve_config_meta_by_model_id(&self, model_id: &str) -> Option<ResolvedProviderMeta> {
        for model_entry in self.models.values() {
            if model_entry.model_id == model_id {
                if let Some(provider_entry) = self.providers.get(&model_entry.provider) {
                    return Some(config_to_resolved(model_entry, provider_entry));
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn config_to_resolved(model: &ConfigModel, provider: &ConfigProvider) -> ResolvedProviderMeta {
    ResolvedProviderMeta {
        kind: match provider.protocol {
            ProviderProtocol::Anthropic => ProviderKind::Anthropic,
            ProviderProtocol::OpenAiCompat => ProviderKind::OpenAi,
        },
        api_key_env: provider.api_key_env.clone(),
        base_url: provider.base_url.clone(),
        base_url_env: provider.base_url_env.clone(),
        display_name: provider
            .display_name
            .clone()
            .or_else(|| Some(model.provider.clone())),
    }
}

fn hardcoded_to_resolved(meta: ProviderMetadata) -> ResolvedProviderMeta {
    ResolvedProviderMeta {
        kind: meta.provider,
        api_key_env: Some(meta.auth_env.to_string()),
        base_url: None,
        base_url_env: Some(meta.base_url_env.to_string()),
        display_name: None,
    }
}

/// Leak a string so it lives for `'static`.  Used to bridge config values
/// into the `OpenAiCompatConfig` type which requires `&'static str` fields.
/// Safe because provider configs are loaded once at process start.
fn leak_string(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_registry() -> ProviderRegistry {
        let mut providers = BTreeMap::new();
        providers.insert(
            "my-ollama".to_string(),
            ConfigProvider {
                protocol: ProviderProtocol::OpenAiCompat,
                api_key_env: None,
                base_url: Some("http://localhost:11434/v1".to_string()),
                base_url_env: None,
                display_name: Some("Ollama".to_string()),
            },
        );
        providers.insert(
            "anthropic".to_string(),
            ConfigProvider {
                protocol: ProviderProtocol::Anthropic,
                api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                base_url: None,
                base_url_env: Some("ANTHROPIC_BASE_URL".to_string()),
                display_name: None,
            },
        );

        let mut models = BTreeMap::new();
        models.insert(
            "local".to_string(),
            ConfigModel {
                provider: "my-ollama".to_string(),
                model_id: "llama3.2:8b".to_string(),
                max_output_tokens: Some(16_384),
                context_window: Some(128_000),
            },
        );
        models.insert(
            "fast".to_string(),
            ConfigModel {
                provider: "anthropic".to_string(),
                model_id: "claude-sonnet-4-6".to_string(),
                max_output_tokens: None,
                context_window: None,
            },
        );

        ProviderRegistry::new(providers, models)
    }

    #[test]
    fn config_model_alias_resolves() {
        let reg = sample_registry();
        assert_eq!(reg.resolve_model_alias("local"), "llama3.2:8b");
        assert_eq!(reg.resolve_model_alias("fast"), "claude-sonnet-4-6");
    }

    #[test]
    fn falls_back_to_hardcoded_alias() {
        let reg = sample_registry();
        assert_eq!(reg.resolve_model_alias("opus"), "claude-opus-4-6");
        assert_eq!(reg.resolve_model_alias("grok"), "grok-3");
    }

    #[test]
    fn config_metadata_resolves_with_display_name() {
        let reg = sample_registry();
        let meta = reg
            .metadata_for_model("local")
            .expect("config model should resolve");
        assert_eq!(meta.kind, ProviderKind::OpenAi);
        assert_eq!(meta.base_url.as_deref(), Some("http://localhost:11434/v1"));
        assert_eq!(meta.display_name.as_deref(), Some("Ollama"));
    }

    #[test]
    fn config_token_limits() {
        let reg = sample_registry();
        let limit = reg
            .model_token_limit("local")
            .expect("config model should have limits");
        assert_eq!(limit.max_output_tokens, 16_384);
        assert_eq!(limit.context_window_tokens, 128_000);
    }

    #[test]
    fn config_model_without_limits_falls_back_to_hardcoded() {
        let reg = sample_registry();
        // "fast" maps to claude-sonnet-4-6 which has hardcoded limits
        let limit = reg
            .model_token_limit("fast")
            .expect("should fall back to hardcoded limits");
        assert_eq!(limit.max_output_tokens, 64_000);
        assert_eq!(limit.context_window_tokens, 200_000);
    }

    #[test]
    fn empty_registry_falls_back_entirely() {
        let reg = ProviderRegistry::default();
        assert_eq!(reg.resolve_model_alias("opus"), "claude-opus-4-6");
        assert!(reg.metadata_for_model("claude-opus-4-6").is_some());
        assert!(reg.is_config_empty());
    }

    #[test]
    fn detect_provider_kind_uses_config() {
        let reg = sample_registry();
        assert_eq!(reg.detect_provider_kind("local"), ProviderKind::OpenAi);
        assert_eq!(reg.detect_provider_kind("fast"), ProviderKind::Anthropic);
        // Hardcoded fallback
        assert_eq!(reg.detect_provider_kind("grok-3"), ProviderKind::Xai);
    }

    #[test]
    fn openai_compat_config_for_builds_correctly() {
        let provider = ConfigProvider {
            protocol: ProviderProtocol::OpenAiCompat,
            api_key_env: Some("MY_KEY".to_string()),
            base_url: Some("http://localhost:8080/v1".to_string()),
            base_url_env: None,
            display_name: Some("LocalLLM".to_string()),
        };
        let config =
            ProviderRegistry::openai_compat_config_for(&provider).expect("should build config");
        assert_eq!(config.api_key_env, "MY_KEY");
        assert_eq!(config.default_base_url, "http://localhost:8080/v1");
        assert_eq!(config.provider_name, "LocalLLM");
    }

    #[test]
    fn openai_compat_config_returns_none_for_anthropic() {
        let provider = ConfigProvider {
            protocol: ProviderProtocol::Anthropic,
            api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
            base_url: None,
            base_url_env: None,
            display_name: None,
        };
        assert!(ProviderRegistry::openai_compat_config_for(&provider).is_none());
    }
}
