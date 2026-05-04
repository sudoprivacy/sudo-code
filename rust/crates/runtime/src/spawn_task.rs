//! Managed-agent loop spawn entry — v0 echo scaffolding.
//!
//! Wires the per-pid agent loop into a sudocode-driven body that polls
//! `/proc/{pid}/chat-with-me` for inbound JSON envelopes and writes
//! responses back through the same mailbox path. The full LLM
//! turn-driver wiring (constructing a [`crate::ConversationRuntime`]
//! with a provider client + tool executor and calling `run_turn` per
//! inbound prompt) lands in a follow-up PR — v0 ships the
//! scaffolding (procfs poll loop, [`crate::HookAbortSignal`] plumbing,
//! envelope round-trip pass-through) so the nexus-side
//! `ManagedAgentService` wiring + e2e round-trip can land alongside.
//!
//! Cancellation: callers reuse [`crate::HookAbortSignal`] (the same
//! signal `with_hook_abort_signal` threads into a
//! [`crate::ConversationRuntime`] when v1 lands). At the nexus
//! boundary, `cancel(Turn)` and `cancel(Session)` both translate to
//! `abort_signal.abort()`; the difference falls out of the loop body
//! once the v1 `run_turn` driver lands. v0's body has no per-turn
//! boundary, so today turn-cancel and session-cancel both terminate
//! the loop on the next poll iteration.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kernel::core::agents::registry::AgentDescriptor;
use kernel::kernel::{Kernel, OperationContext};

use crate::hooks::HookAbortSignal;

/// Sleep between `sys_read` polls when the canonical mailbox is idle.
/// Kept short so prompt-to-response latency stays bounded by a single
/// sleep tick once the v1 LLM body lands; today's echo body returns
/// even faster because each iteration writes the reply inline.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Per-call `sys_read` blocking timeout. `0` keeps the call
/// non-blocking — `FileWatchRegistry::wait_for_event` is a stub today
/// (returns `None` immediately), so any non-zero timeout would
/// degrade to a busy wait inside the kernel. Once `sys_watch` lands
/// the loop can drop the explicit `thread::sleep` below and let the
/// kernel block for it.
const READ_TIMEOUT_MS: u64 = 0;

/// Handle returned by [`spawn_task`]. Caller (typically nexus's
/// `ManagedAgentService::start_session`) holds this so cancel paths
/// can call `abort_signal.abort()` without caring whether the per-pid
/// task is mid-turn or idle in the poll loop, and so observability
/// code can wait for the loop to actually leave by joining the
/// thread.
pub struct SpawnHandle {
    /// Shared abort signal — the same [`HookAbortSignal`] the v1 LLM
    /// runtime threads through to [`crate::ConversationRuntime`] via
    /// `with_hook_abort_signal`. v0's loop checks `is_aborted()`
    /// between poll iterations.
    pub abort_signal: HookAbortSignal,
    /// Join handle for the spawned worker thread. v0 uses
    /// [`std::thread`] because the body has no async work; v1 will
    /// switch this to a `tokio::task::JoinHandle` so the LLM stream
    /// can run inside `spawn_blocking` and bubble structured errors
    /// up through the join surface.
    pub join: thread::JoinHandle<()>,
}

/// Spawn the managed-agent loop for a freshly-allocated pid.
///
/// `desc` is the descriptor nexus's
/// `ManagedAgentService::start_session` already planted in
/// `AgentRegistry`; we read `pid` (which procfs path to poll),
/// `name` (the agent_id we stamp our writes with), `owner_id` and
/// `zone_id` (carried into the per-call `OperationContext`).
///
/// `kernel` is the same `Arc<Kernel>` the service holds — every
/// `sys_read` / `sys_write` rides through it as a system-tier call
/// (`is_system = true` so workspace-boundary checks pass).
///
/// v0 body: for each inbound envelope where `from != desc.name`,
/// write back `{"to": <inbound from>, "from": <self>, "body":
/// "echo: <inbound body>"}`. The explicit self-`from` lets the loop
/// guard skip our own writes without depending on
/// `MailboxStampingHook` being installed (kernel can run
/// without the hook in unit tests). When the hook is installed the
/// stamp is a no-op because the field already matches.
///
/// v1 body (future PR): construct [`crate::ConversationRuntime`] +
/// call `run_turn` per inbound prompt. The [`HookAbortSignal`]
/// returned in the handle is the wiring point —
/// `with_hook_abort_signal(handle.abort_signal.clone())` on the
/// runtime gives turn-level abort the same wire that session-level
/// abort already uses.
#[must_use]
pub fn spawn_task(kernel: Arc<Kernel>, desc: AgentDescriptor) -> SpawnHandle {
    let abort_signal = HookAbortSignal::default();
    let abort_for_thread = abort_signal.clone();

    let join = thread::Builder::new()
        .name(format!("managed-agent-{}", desc.pid))
        .spawn(move || {
            run_loop(&kernel, &desc, &abort_for_thread);
        })
        .expect("OS refused to spawn managed-agent thread");

    SpawnHandle { abort_signal, join }
}

fn run_loop(kernel: &Arc<Kernel>, desc: &AgentDescriptor, abort: &HookAbortSignal) {
    let cwm_path = format!("/proc/{}/chat-with-me", desc.pid);
    let agent_id = desc.name.as_str();
    let ctx = OperationContext::new(
        &desc.owner_id,
        &desc.zone_id,
        /* is_admin */ false,
        Some(agent_id),
        /* is_system */ true,
    );

    let mut next_offset: u64 = 0;
    while !abort.is_aborted() {
        match kernel.sys_read(&cwm_path, &ctx, READ_TIMEOUT_MS, next_offset) {
            Ok(result) => {
                if let Some(bytes) = result.data.as_ref() {
                    if !bytes.is_empty() {
                        if let Some(reply) = build_echo_reply(bytes, agent_id) {
                            // Pass-through if write fails — v0 does
                            // not retry; the abort path wins on the
                            // next poll. Real failure modes (mailbox
                            // capacity exceeded, federation lost
                            // quorum) are surfaced when the v1 LLM
                            // body wraps writes with structured
                            // RuntimeError reporting.
                            let _ = kernel.sys_write(&cwm_path, &ctx, &reply, 0);
                        }
                    }
                }
                if let Some(advanced) = result.stream_next_offset {
                    next_offset = advanced as u64;
                }
            }
            Err(_) => {
                // Path tear-down (cancel(Session) → procfs unregister)
                // arrives as FileNotFound; other transient kernel
                // errors share the same path. v0 treats every kernel
                // error as terminal because the loop's lifetime is
                // bounded by the pid's procfs subtree anyway.
                break;
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Build the echo response envelope, or `None` when the inbound
/// envelope should be skipped (own write, no `from` field, non-JSON,
/// non-object).
fn build_echo_reply(inbound: &[u8], self_agent_id: &str) -> Option<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_slice(inbound).ok()?;
    let obj = value.as_object()?;
    let from = obj.get("from").and_then(|v| v.as_str())?;
    if from == self_agent_id {
        return None;
    }
    let body = obj.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let reply = serde_json::json!({
        "to": from,
        "from": self_agent_id,
        "body": format!("echo: {body}"),
    });
    serde_json::to_vec(&reply).ok()
}

// Tests live under `runtime/tests/spawn_task.rs` as an integration
// test binary so they can compile without bringing in the rest of
// the lib's test target (which has pre-existing platform-specific
// fixtures unrelated to spawn_task).
