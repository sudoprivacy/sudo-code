mod client;
mod error;
mod http_client;
mod prompt_cache;
mod providers;
mod sse;
mod types;

pub use client::{
    base_url_for_mode, oauth_token_is_expired, read_base_url, read_xai_base_url,
    resolve_saved_oauth_token, resolve_startup_auth_source, MessageStream, OAuthTokenSet,
    ProviderClient,
};
pub use error::ApiError;
pub use http_client::{
    build_http_client, build_http_client_or_default, build_http_client_with, ProxyConfig,
};
pub use prompt_cache::{
    CacheBreakEvent, PromptCache, PromptCacheConfig, PromptCachePaths, PromptCacheRecord,
    PromptCacheStats,
};
pub use providers::anthropic::{
    is_anthropic_api_key, is_claude_code_oauth_token, is_proxy_auth_token, AnthropicClient,
    AnthropicClient as ApiClient, AuthSource, DEFAULT_BASE_URL,
};
pub use providers::codex::{CodexClient, DEFAULT_CODEX_BASE_URL};
pub use providers::openai_compat::{
    build_chat_completion_request, flatten_tool_result_content, is_reasoning_model,
    model_rejects_is_error_field, translate_message, OpenAiCompatClient, OpenAiCompatConfig,
};
pub use providers::registry::{
    resolve_model, resolve_provider_from_config, ApiFormat, Credential, ModelConfigEntry,
    ModelProviderMapping, ProviderConnectionConfig, ResolvedProvider, SudoCodeConfig,
};
pub use providers::{
    detect_provider_kind, max_tokens_for_model, max_tokens_for_model_with_override,
    resolve_model_alias, AuthMode, ProviderKind,
};
pub use sse::{parse_frame, SseParser};
pub use types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent, MessageRequest,
    MessageResponse, MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent,
    ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};

pub use telemetry::{
    AnalyticsEvent, AnthropicRequestProfile, ClientIdentity, JsonlTelemetrySink,
    MemoryTelemetrySink, SessionTraceRecord, SessionTracer, TelemetryEvent, TelemetrySink,
    DEFAULT_ANTHROPIC_VERSION,
};
