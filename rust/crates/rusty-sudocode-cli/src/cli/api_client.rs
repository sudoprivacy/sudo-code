use std::io::{self, Write};

use api::{
    resolve_startup_auth_source, AnthropicClient, AuthMode, AuthSource, ContentBlockDelta,
    ImageSource, InputContentBlock, InputMessage, MessageRequest, MessageResponse,
    OutputContentBlock, PromptCache, ProviderClient as ApiProviderClient,
    StreamEvent as ApiStreamEvent, SystemCacheBlock, ToolChoice, ToolDefinition,
    ToolResultContentBlock,
};
use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ContentBlock, ConversationMessage, MessageRole,
    PromptCacheEvent, RuntimeError, TokenUsage,
};
use telemetry::{JsonlTelemetrySink, SessionTracer};
use tools::GlobalToolRegistry;

use super::format::{format_tool_call_start, format_user_visible_api_error};
use crate::render::{MarkdownStreamState, TerminalRenderer};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{
    AllowedToolSet, InternalPromptProgressReporter, RuntimeConfig, POST_TOOL_STALL_TIMEOUT,
};

// NOTE: Despite the historical name `AnthropicRuntimeClient`, this struct
// now holds an `ApiProviderClient` which dispatches to Anthropic, xAI,
// OpenAI, or DashScope at construction time based on
// `detect_provider_kind(&model)`. The struct name is kept to avoid
// churning `BuiltRuntime` and every Deref/DerefMut site that references
// it. See ROADMAP #29 for the provider-dispatch routing fix.
pub(crate) struct AnthropicRuntimeClient {
    pub(crate) runtime: tokio::runtime::Runtime,
    pub(crate) client: ApiProviderClient,
    pub(crate) session_id: String,
    pub(crate) model: String,
    pub(crate) enable_tools: bool,
    pub(crate) emit_output: bool,
    pub(crate) allowed_tools: Option<AllowedToolSet>,
    pub(crate) tool_registry: GlobalToolRegistry,
    pub(crate) progress_reporter: Option<InternalPromptProgressReporter>,
    pub(crate) reasoning_effort: Option<String>,
    /// Shared flag from the Spinner. Set to `true` before writing output to
    /// pause the spinner animation, `false` after to let it resume.
    pub(crate) spinner_pause: Option<Arc<AtomicBool>>,
}

impl AnthropicRuntimeClient {
    pub(crate) fn new(
        session_id: &str,
        config: &RuntimeConfig,
        tool_registry: GlobalToolRegistry,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let sudocode_config = &config.sudocode_config;
        let effective_mode = config.auth_mode;

        let resolved = api::resolve_provider_from_config(
            &config.model,
            Some(effective_mode),
            sudocode_config,
        )?;
        let mut client = ApiProviderClient::from_resolved(&resolved, Some(effective_mode))?
            .with_prompt_cache(PromptCache::new(session_id));

        if let Ok(capture_path) = std::env::var("SCODE_HTTP_DEBUG") {
            let sink = Arc::new(JsonlTelemetrySink::new(&capture_path)?);
            let tracer = SessionTracer::new(session_id, sink);
            client = client.with_session_tracer(tracer);
        }

        Ok(Self {
            runtime: tokio::runtime::Runtime::new()?,
            client,
            session_id: session_id.to_string(),
            model: config.model.clone(),
            enable_tools: config.enable_tools,
            emit_output: config.emit_output,
            allowed_tools: config.allowed_tools.clone(),
            tool_registry,
            progress_reporter: config.progress_reporter.clone(),
            reasoning_effort: None,
            spinner_pause: None,
        })
    }

    pub(crate) fn set_spinner_pause(&mut self, flag: Arc<AtomicBool>) {
        self.spinner_pause = Some(flag);
    }

    /// Pause the spinner and clear its line before writing content.
    fn pause_spinner(&self) {
        if let Some(flag) = &self.spinner_pause {
            flag.store(true, Ordering::SeqCst);
            // Brief sleep to let the spinner thread finish its current tick.
            std::thread::sleep(std::time::Duration::from_millis(10));
            // Clear the spinner text from the current line.
            let _ = write!(io::stdout(), "\r\x1b[2K");
            let _ = io::stdout().flush();
        }
    }

    /// Resume the spinner after content has been written.
    fn resume_spinner(&self) {
        if let Some(flag) = &self.spinner_pause {
            flag.store(false, Ordering::SeqCst);
        }
    }

    pub(crate) fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }
}

pub(crate) fn resolve_cli_auth_source() -> Result<AuthSource, Box<dyn std::error::Error>> {
    Ok(resolve_cli_auth_source_for_cwd()?)
}

pub(crate) fn resolve_cli_auth_source_for_cwd() -> Result<AuthSource, api::ApiError> {
    resolve_startup_auth_source(|| Ok(None))
}

impl ApiClient for AnthropicRuntimeClient {
    #[allow(clippy::too_many_lines)]
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        if let Some(progress_reporter) = &self.progress_reporter {
            progress_reporter.mark_model_phase();
        }
        let is_post_tool = request_ends_with_tool_result(&request);
        let system_cache_blocks = (!request.system_prompt.is_empty()).then(|| {
            vec![
                SystemCacheBlock {
                    text: request.system_prompt.static_text(),
                    cache_scope: Some("global".to_string()),
                },
                SystemCacheBlock {
                    text: request.system_prompt.dynamic_text(),
                    cache_scope: None,
                },
            ]
        });
        let message_request = MessageRequest {
            model: self.model.clone(),
            max_tokens: max_tokens_for_model(&self.model),
            messages: convert_messages(&request.messages),
            system: (!request.system_prompt.is_empty()).then(|| request.system_prompt.render()),
            tools: self
                .enable_tools
                .then(|| filter_tool_specs(&self.tool_registry, self.allowed_tools.as_ref())),
            tool_choice: self.enable_tools.then_some(ToolChoice::Auto),
            stream: true,
            reasoning_effort: self.reasoning_effort.clone(),
            system_cache_blocks,
            ..Default::default()
        };

        self.runtime.block_on(async {
            // When resuming after tool execution, apply a stall timeout on the
            // first stream event.  If the model does not respond within the
            // deadline we drop the stalled connection and re-send the request as
            // a continuation nudge (one retry only).
            let max_attempts: usize = if is_post_tool { 2 } else { 1 };

            for attempt in 1..=max_attempts {
                let result = self
                    .consume_stream(&message_request, is_post_tool && attempt == 1)
                    .await;
                match result {
                    Ok(events) => return Ok(events),
                    Err(error)
                        if error.to_string().contains("post-tool stall")
                            && attempt < max_attempts =>
                    {
                        // Stalled after tool completion — nudge the model by
                        // re-sending the same request.
                    }
                    Err(error) => return Err(error),
                }
            }

            Err(RuntimeError::new("post-tool continuation nudge exhausted"))
        })
    }
}

impl AnthropicRuntimeClient {
    /// Consume a single streaming response, optionally applying a stall
    /// timeout on the first event for post-tool continuations.
    #[allow(clippy::too_many_lines)]
    async fn consume_stream(
        &self,
        message_request: &MessageRequest,
        apply_stall_timeout: bool,
    ) -> Result<Vec<AssistantEvent>, RuntimeError> {
        let mut stream = self
            .client
            .stream_message(message_request)
            .await
            .map_err(|error| {
                RuntimeError::new(format_user_visible_api_error(&self.session_id, &error))
            })?;
        let mut stdout = io::stdout();
        let mut sink = io::sink();
        let out: &mut dyn Write = if self.emit_output {
            &mut stdout
        } else {
            &mut sink
        };
        let renderer = TerminalRenderer::new();
        let mut markdown_stream = MarkdownStreamState::default();
        let mut events = Vec::new();
        let mut pending_tool: Option<(String, String, String, Option<String>)> = None;
        let mut block_has_thinking_summary = false;
        let mut saw_stop = false;
        let mut received_any_event = false;
        let mut glyph_state = ResponseGlyphState::new(query_terminal_width());

        loop {
            let next = if apply_stall_timeout && !received_any_event {
                match tokio::time::timeout(POST_TOOL_STALL_TIMEOUT, stream.next_event()).await {
                    Ok(inner) => inner.map_err(|error| {
                        RuntimeError::new(format_user_visible_api_error(&self.session_id, &error))
                    })?,
                    Err(_elapsed) => {
                        return Err(RuntimeError::new(
                            "post-tool stall: model did not respond within timeout",
                        ));
                    }
                }
            } else {
                stream.next_event().await.map_err(|error| {
                    RuntimeError::new(format_user_visible_api_error(&self.session_id, &error))
                })?
            };

            let Some(event) = next else {
                break;
            };
            received_any_event = true;

            match event {
                ApiStreamEvent::MessageStart(start) => {
                    for block in start.message.content {
                        push_output_block(
                            block,
                            out,
                            &mut events,
                            &mut pending_tool,
                            true,
                            &mut block_has_thinking_summary,
                            &mut glyph_state,
                        )?;
                    }
                }
                ApiStreamEvent::ContentBlockStart(start) => {
                    push_output_block(
                        start.content_block,
                        out,
                        &mut events,
                        &mut pending_tool,
                        true,
                        &mut block_has_thinking_summary,
                        &mut glyph_state,
                    )?;
                }
                ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                    ContentBlockDelta::TextDelta { text } => {
                        if !text.is_empty() {
                            if let Some(progress_reporter) = &self.progress_reporter {
                                progress_reporter.mark_text_phase(&text);
                            }
                            if let Some(rendered) = markdown_stream.push(&renderer, &text) {
                                self.pause_spinner();
                                let prefixed = glyph_state.apply(&rendered);
                                write!(out, "{prefixed}")
                                    .and_then(|()| out.flush())
                                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                            }
                            events.push(AssistantEvent::TextDelta(text));
                        }
                    }
                    ContentBlockDelta::InputJsonDelta { partial_json } => {
                        if let Some((_, _, input, _)) = &mut pending_tool {
                            input.push_str(&partial_json);
                        }
                    }
                    ContentBlockDelta::ThinkingDelta { .. } => {
                        if !block_has_thinking_summary {
                            self.pause_spinner();
                            render_thinking_block_summary(out, None, false)?;
                            block_has_thinking_summary = true;
                            glyph_state.visible_col = 0;
                        }
                    }
                    ContentBlockDelta::SignatureDelta { .. } => {}
                },
                ApiStreamEvent::ContentBlockStop(_) => {
                    block_has_thinking_summary = false;
                    if let Some(rendered) = markdown_stream.flush(&renderer) {
                        let prefixed = glyph_state.apply(&rendered);
                        write!(out, "{prefixed}")
                            .and_then(|()| out.flush())
                            .map_err(|error| RuntimeError::new(error.to_string()))?;
                    }
                    if let Some((id, name, input, thought_signature)) = pending_tool.take() {
                        if let Some(progress_reporter) = &self.progress_reporter {
                            progress_reporter.mark_tool_phase(&name, &input);
                        }
                        self.pause_spinner();
                        writeln!(out, "\n{}", format_tool_call_start(&name, &input))
                            .and_then(|()| out.flush())
                            .map_err(|error| RuntimeError::new(error.to_string()))?;
                        glyph_state.visible_col = 0;
                        // Resume spinner so it shows during tool execution.
                        self.resume_spinner();
                        events.push(AssistantEvent::ToolUse {
                            id,
                            name,
                            input,
                            thought_signature,
                        });
                    }
                }
                ApiStreamEvent::MessageDelta(delta) => {
                    events.push(AssistantEvent::Usage(delta.usage.token_usage()));
                }
                ApiStreamEvent::MessageStop(_) => {
                    saw_stop = true;
                    if let Some(rendered) = markdown_stream.flush(&renderer) {
                        let prefixed = glyph_state.apply(&rendered);
                        write!(out, "{prefixed}")
                            .and_then(|()| out.flush())
                            .map_err(|error| RuntimeError::new(error.to_string()))?;
                    }
                    events.push(AssistantEvent::MessageStop);
                }
            }
        }

        push_prompt_cache_record(&self.client, &mut events);

        if !saw_stop
            && events.iter().any(|event| {
                matches!(event, AssistantEvent::TextDelta(text) if !text.is_empty())
                    || matches!(event, AssistantEvent::ToolUse { .. })
            })
        {
            events.push(AssistantEvent::MessageStop);
        }

        if events
            .iter()
            .any(|event| matches!(event, AssistantEvent::MessageStop))
        {
            return Ok(events);
        }

        let response = self
            .client
            .send_message(&MessageRequest {
                stream: false,
                ..message_request.clone()
            })
            .await
            .map_err(|error| {
                RuntimeError::new(format_user_visible_api_error(&self.session_id, &error))
            })?;
        let mut events = response_to_events(response, out)?;
        push_prompt_cache_record(&self.client, &mut events);
        Ok(events)
    }
}

/// Returns `true` when the conversation ends with a tool-result message,
/// meaning the model is expected to continue after tool execution.
pub(crate) fn request_ends_with_tool_result(request: &ApiRequest) -> bool {
    request
        .messages
        .last()
        .is_some_and(|message| message.role == MessageRole::Tool)
}

pub(crate) fn final_assistant_text(summary: &runtime::TurnSummary) -> String {
    summary
        .assistant_messages
        .last()
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

pub(crate) fn collect_tool_uses(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .assistant_messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                id, name, input, ..
            } => Some(serde_json::json!({
                "id": id,
                "name": name,
                "input": input,
            })),
            _ => None,
        })
        .collect()
}

pub(crate) fn collect_tool_results(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .tool_results
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } => Some(serde_json::json!({
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "output": output,
                "is_error": is_error,
            })),
            _ => None,
        })
        .collect()
}

pub(crate) fn collect_prompt_cache_events(
    summary: &runtime::TurnSummary,
) -> Vec<serde_json::Value> {
    summary
        .prompt_cache_events
        .iter()
        .map(|event| {
            serde_json::json!({
                "unexpected": event.unexpected,
                "reason": event.reason,
                "previous_cache_read_input_tokens": event.previous_cache_read_input_tokens,
                "current_cache_read_input_tokens": event.current_cache_read_input_tokens,
                "token_drop": event.token_drop,
            })
        })
        .collect()
}

pub(crate) fn max_tokens_for_model(model: &str) -> u32 {
    if model.contains("opus") {
        32_000
    } else {
        64_000
    }
}

pub(crate) fn render_thinking_block_summary(
    out: &mut (impl Write + ?Sized),
    char_count: Option<usize>,
    redacted: bool,
) -> Result<(), RuntimeError> {
    let summary = if redacted {
        "\n  ▶ Thinking block hidden by provider\n".to_string()
    } else if let Some(char_count) = char_count {
        format!("\n  ▶ Thinking ({char_count} chars hidden)\n")
    } else {
        "\n  ▶ Thinking hidden\n".to_string()
    };
    write!(out, "{summary}")
        .and_then(|()| out.flush())
        .map_err(|error| RuntimeError::new(error.to_string()))
}

/// Stateful processor that prefixes the first line with ⏺ (bold) and indents
/// all continuation lines by two spaces so that column 0 is reserved
/// exclusively for status glyphs. Hard-wraps text at the terminal width so
/// the terminal never soft-wraps into column 0.
pub(crate) struct ResponseGlyphState {
    started: bool,
    visible_col: usize,
    max_col: usize,
    in_escape: bool,
}

impl ResponseGlyphState {
    fn new(terminal_width: usize) -> Self {
        Self {
            started: false,
            visible_col: 0,
            // Ensure at least 4 columns to avoid degenerate wrapping.
            max_col: terminal_width.max(4),
            in_escape: false,
        }
    }

    /// Process a rendered ANSI text chunk. Returns the wrapped+margined output.
    fn apply(&mut self, rendered: &str) -> String {
        if rendered.is_empty() {
            return String::new();
        }

        let mut out = String::with_capacity(rendered.len() + 64);

        for ch in rendered.chars() {
            if ch == '\r' {
                out.push(ch);
                self.visible_col = 0;
                continue;
            }
            if ch == '\n' {
                out.push(ch);
                self.visible_col = 0;
                continue;
            }

            // At line start, emit glyph or margin.
            if self.visible_col == 0 {
                if self.started {
                    out.push_str("  ");
                } else {
                    self.started = true;
                    out.push_str("\r\x1b[2K\x1b[1m⏺\x1b[0m ");
                }
                self.visible_col = 2;
            }

            // ANSI escape start.
            if ch == '\x1b' {
                self.in_escape = true;
                out.push(ch);
                continue;
            }

            // Inside an ANSI CSI sequence — push until ASCII letter terminates.
            if self.in_escape {
                out.push(ch);
                if ch.is_ascii_alphabetic() {
                    self.in_escape = false;
                }
                continue;
            }

            // Hard wrap: line has reached the terminal edge.
            if self.visible_col >= self.max_col {
                out.push('\n');
                out.push_str("  ");
                self.visible_col = 2;
            }

            out.push(ch);
            self.visible_col += 1;
        }

        out
    }
}

fn query_terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(cols, _)| cols as usize)
        .unwrap_or(80)
}

pub(crate) fn push_output_block(
    block: OutputContentBlock,
    out: &mut (impl Write + ?Sized),
    events: &mut Vec<AssistantEvent>,
    pending_tool: &mut Option<(String, String, String, Option<String>)>,
    streaming_tool_input: bool,
    block_has_thinking_summary: &mut bool,
    glyph_state: &mut ResponseGlyphState,
) -> Result<(), RuntimeError> {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                let rendered = TerminalRenderer::new().markdown_to_ansi(&text);
                let prefixed = glyph_state.apply(&rendered);
                write!(out, "{prefixed}")
                    .and_then(|()| out.flush())
                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse {
            id,
            name,
            input,
            thought_signature,
        } => {
            // During streaming, the initial content_block_start has an empty input ({}).
            // The real input arrives via input_json_delta events. In
            // non-streaming responses, preserve a legitimate empty object.
            let initial_input = if streaming_tool_input
                && input.is_object()
                && input.as_object().is_some_and(serde_json::Map::is_empty)
            {
                String::new()
            } else {
                input.to_string()
            };
            *pending_tool = Some((id, name, initial_input, thought_signature));
        }
        OutputContentBlock::Thinking { thinking, .. } => {
            render_thinking_block_summary(out, Some(thinking.chars().count()), false)?;
            *block_has_thinking_summary = true;
            glyph_state.visible_col = 0;
        }
        OutputContentBlock::RedactedThinking { .. } => {
            render_thinking_block_summary(out, None, true)?;
            *block_has_thinking_summary = true;
            glyph_state.visible_col = 0;
        }
    }
    Ok(())
}

pub(crate) fn response_to_events(
    response: MessageResponse,
    out: &mut (impl Write + ?Sized),
) -> Result<Vec<AssistantEvent>, RuntimeError> {
    let mut events = Vec::new();
    let mut pending_tool = None;
    let mut glyph_state = ResponseGlyphState::new(query_terminal_width());

    for block in response.content {
        let mut block_has_thinking_summary = false;
        push_output_block(
            block,
            out,
            &mut events,
            &mut pending_tool,
            false,
            &mut block_has_thinking_summary,
            &mut glyph_state,
        )?;
        if let Some((id, name, input, thought_signature)) = pending_tool.take() {
            events.push(AssistantEvent::ToolUse {
                id,
                name,
                input,
                thought_signature,
            });
        }
    }

    events.push(AssistantEvent::Usage(response.usage.token_usage()));
    events.push(AssistantEvent::MessageStop);
    Ok(events)
}

pub(crate) fn push_prompt_cache_record(
    client: &ApiProviderClient,
    events: &mut Vec<AssistantEvent>,
) {
    // `ApiProviderClient::take_last_prompt_cache_record` is a pass-through
    // to the Anthropic variant and returns `None` for OpenAI-compat /
    // xAI variants, which do not have a prompt cache. So this helper
    // remains a no-op on non-Anthropic providers without any extra
    // branching here.
    if let Some(record) = client.take_last_prompt_cache_record() {
        if let Some(event) = prompt_cache_record_to_runtime_event(record) {
            events.push(AssistantEvent::PromptCache(event));
        }
    }
}

pub(crate) fn prompt_cache_record_to_runtime_event(
    record: api::PromptCacheRecord,
) -> Option<PromptCacheEvent> {
    let cache_break = record.cache_break?;
    Some(PromptCacheEvent {
        unexpected: cache_break.unexpected,
        reason: cache_break.reason,
        previous_cache_read_input_tokens: cache_break.previous_cache_read_input_tokens,
        current_cache_read_input_tokens: cache_break.current_cache_read_input_tokens,
        token_drop: cache_break.token_drop,
    })
}

pub(crate) fn convert_messages(messages: &[ConversationMessage]) -> Vec<InputMessage> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::System | MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "assistant",
            };
            let content = message
                .blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => InputContentBlock::Text { text: text.clone() },
                    ContentBlock::Image { data, mime_type } => InputContentBlock::Image {
                        source: ImageSource {
                            source_type: "base64".to_string(),
                            media_type: mime_type.clone(),
                            data: data.clone(),
                        },
                    },
                    ContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        thought_signature,
                    } => InputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::from_str(input)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": input })),
                        thought_signature: thought_signature.clone(),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    } => InputContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContentBlock::Text {
                            text: output.clone(),
                        }],
                        is_error: *is_error,
                    },
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}

pub(crate) fn filter_tool_specs(
    tool_registry: &GlobalToolRegistry,
    allowed_tools: Option<&AllowedToolSet>,
) -> Vec<ToolDefinition> {
    tool_registry.definitions(allowed_tools)
}
