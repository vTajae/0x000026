# Anti-Slop Hardening: Implementation Workflow

**Generated:** 2026-03-03
**Branch:** `custom/integrations`
**Based on:** `claudedocs/research_anti_slop_architecture_audit_2026-03-03.md`
**Goal:** Close 5 enforcement gaps identified in the blueprint compliance audit

---

## Pre-Implementation Notes

### Audit Correction: Approval System (#2)

The audit scored approval gates as **CRITICAL** based on the default trait methods in
`kernel_handle.rs:144-160`. However, deeper code analysis reveals the **kernel overrides
both methods** at `kernel.rs:5445-5482` with a fully functional implementation:

- `requires_approval()` delegates to `ApprovalManager::requires_approval()`
- `request_approval()` creates typed `ApprovalRequest` with risk classification
- `ApprovalManager` at `kernel/src/approval.rs` has real risk classification:
  - `shell_exec` = Critical, `file_write`/`file_delete` = High,
    `web_fetch`/`browser_navigate` = Medium, everything else = Low
- `tool_runner.rs:134-154` has the approval gate wired in the execution path
- Dashboard UI exists at `static/js/pages/approvals.js`
- API routes exist at `routes.rs:8087-8241` (GET/POST/approve/reject)
- Hot-reload support via `HotAction::UpdateApprovalPolicy`

**The approval system is fully implemented.** The only question is whether the default
`ApprovalPolicy` enables it (i.e., whether `auto_approve` defaults to `false`).

**Revised scope for Fix #2:** Verify default config values. If `auto_approve` defaults
to `true` or `require_approval` defaults to empty, flip those defaults. No new code
needed — just a Default impl adjustment.

---

## Phase 1: Input Injection Filtering

**Severity:** HIGH | **Risk:** LOW | **Estimated changes:** 2 files, ~15 lines
**Dependencies:** None

### Problem

`strip_injection_markers()` at `session_repair.rs:571-612` catches 12 injection
patterns but is only applied to **tool results** via `strip_tool_result_details()`
(called from `compactor.rs:473`).

User input from Discord/API reaches the LLM completely unsanitized:
```
discord.rs:481 → bridge.rs:544 → kernel.rs:2163 → agent_loop.rs (unsanitized)
```

### Implementation Steps

#### Step 1.1: Export the injection filter

**File:** `crates/openfang-runtime/src/session_repair.rs`
- Make `strip_injection_markers()` public (currently `fn`, change to `pub fn`)
- Or create a thin public wrapper `pub fn sanitize_user_input(content: &str) -> String`
  that calls `strip_injection_markers()` (avoids exposing internal naming)

#### Step 1.2: Apply at the bridge dispatch point

**File:** `crates/openfang-channels/src/bridge.rs`
- **After line 543** (text extraction from `message.content`), before line 730 (send to agent)
- Apply `sanitize_user_input()` to the `text` variable
- This is the single chokepoint — ALL channels (Discord, Slack, future) funnel through here

```
// Approximate insertion point (after line 543):
let text = /* extracted from message.content */;
let text = openfang_runtime::session_repair::sanitize_user_input(&text);
```

Note: `bridge.rs` is in `openfang-channels` crate which depends on `openfang-runtime`.
Verify the dependency exists in `crates/openfang-channels/Cargo.toml`. If not, the
function should be moved to `openfang-types` (which all crates depend on) or exposed
through the bridge handle trait.

**Alternative:** If crate dependency is wrong direction, add the filtering in
`kernel.rs:execute_llm_agent()` before line 2163, which is the last point before
`run_agent_loop()`. This is in `openfang-kernel` which already depends on
`openfang-runtime`.

#### Step 1.3: Add tests

**File:** `crates/openfang-runtime/src/session_repair.rs` (existing test module)
- Test: user message with `<|system|>` gets sanitized
- Test: user message with `IGNORE PREVIOUS INSTRUCTIONS` gets sanitized
- Test: normal message passes through unchanged
- Test: mixed content (valid + injection) preserves valid, strips injection

### Validation Checkpoint

```bash
cargo test --workspace -q
cargo clippy --workspace --all-targets -- -D warnings
# Verify: grep for "sanitize_user_input" should show call site + definition
```

---

## Phase 2: Approval Policy Default Verification

**Severity:** CRITICAL (if misconfigured) | **Risk:** LOW | **Estimated changes:** 1 file, ~3 lines
**Dependencies:** None (parallel with Phase 1)

### Problem

If `ApprovalPolicy::default()` sets `auto_approve = true` or `require_approval = []`,
the entire approval system is disabled by default despite being fully implemented.

### Implementation Steps

#### Step 2.1: Read and verify defaults

**File:** `crates/openfang-types/src/approval.rs`
- Read the `Default` impl for `ApprovalPolicy`
- Verify:
  - `auto_approve` defaults to `false`
  - `auto_approve_autonomous` defaults to `false` (or `true` if intended for headless mode)
  - `require_approval` defaults to `["shell_exec"]` at minimum

#### Step 2.2: Fix defaults if needed

If `auto_approve` defaults to `true`:
- Change to `false`
- This is the only change needed — the rest of the system is wired

If `require_approval` defaults to empty:
- Change to `vec!["shell_exec".into()]`

#### Step 2.3: Ensure config.toml documentation

Verify that `~/.openfang/config.toml` examples show:
```toml
[approval]
require_approval = ["shell_exec", "file_delete"]
timeout_secs = 120
auto_approve = false
```

### Validation Checkpoint

```bash
cargo test --workspace -q
# Existing approval tests should still pass
# The behavior change is: approval is ON by default instead of OFF
```

---

## Phase 3: Response-Side Secret Filtering

**Severity:** HIGH | **Risk:** LOW | **Estimated changes:** 1 file, ~40 lines
**Dependencies:** None (parallel with Phase 1 & 2)

### Problem

`agent_loop.rs:692-699` returns `final_response` without any post-processing.
If the LLM echoes API keys, passwords, or tokens, they reach the user verbatim.

URL exfiltration checks exist in `tool_runner.rs` but only for outbound requests,
not for response text.

### Implementation Steps

#### Step 3.1: Create secret redaction function

**File:** `crates/openfang-runtime/src/session_repair.rs` (or new `response_filter.rs`)
- Function: `pub fn redact_secrets(response: &str) -> String`
- Patterns to redact (replace with `[REDACTED]`):
  1. API keys: `sk-[a-zA-Z0-9]{20,}`, `gsk_[a-zA-Z0-9]{20,}`, `xai-[a-zA-Z0-9]{20,}`
  2. Bearer tokens: `Bearer [a-zA-Z0-9._-]{20,}` (in prose, not headers)
  3. Passwords in assignments: `password\s*[=:]\s*["']?[^\s"']{8,}`
  4. AWS keys: `AKIA[A-Z0-9]{16}`
  5. Generic long secrets: `[A-Za-z0-9+/=]{40,}` immediately after `key`, `token`, `secret`, `password` keywords

Keep patterns conservative — better to miss edge cases than false-positive on normal text.

#### Step 3.2: Apply before returning AgentLoopResult

**File:** `crates/openfang-runtime/src/agent_loop.rs`
- **Before line 692** (the `return Ok(AgentLoopResult { ... })`)
- Apply: `let final_response = redact_secrets(&final_response);`

#### Step 3.3: Add tests

- Test: response containing `sk-abc123...` gets redacted
- Test: response containing `AKIAIOSFODNN7EXAMPLE` gets redacted
- Test: response containing `password=hunter2` gets redacted
- Test: normal prose with word "password" is NOT redacted
- Test: short tokens (< 20 chars) are NOT false-positived

### Validation Checkpoint

```bash
cargo test --workspace -q
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Phase 4: Quality Gate Rejection Mode

**Severity:** MEDIUM | **Risk:** MEDIUM | **Estimated changes:** 3 files, ~50 lines
**Dependencies:** None (parallel with above, but test after Phases 1-3)

### Problem

`detect_response_issues()` at `agent_loop.rs:136-176` catches 3 issue types but
the handler at lines 651-669 only stores a critique to procedural memory — the bad
response is returned to the user every time.

### Implementation Steps

#### Step 4.1: Add `ruthless_mode` to LoopGuardConfig

**File:** `crates/openfang-runtime/src/loop_guard.rs`
- Add field to `LoopGuardConfig`:
  ```rust
  /// When true, detected quality issues trigger retry instead of passthrough.
  pub ruthless_mode: bool,
  ```
- Default: `false` (opt-in, non-breaking)
- Add to `Default` impl

#### Step 4.2: Add config field to agent manifest or kernel config

**File:** `crates/openfang-types/src/config.rs` or agent manifest type
- Ensure `ruthless_mode` is configurable per-agent via TOML
- Add `#[serde(default)]` for backwards compatibility

#### Step 4.3: Implement retry logic in agent_loop

**File:** `crates/openfang-runtime/src/agent_loop.rs`
- **At line 651** (after `detect_response_issues()` call):
- If `ruthless_mode` AND issues detected AND retry_count < 2:
  1. Clear `messages` (or reset to system prompt + original user message)
  2. Re-inject user prompt: `messages.push(Message::user(user_message))`
  3. Append quality guidance: a system-level nudge like
     `"Your previous response was rejected: {critique}. Please provide a complete, direct answer."`
  4. Increment `quality_retry_count`
  5. `continue` (restart the loop iteration)
- If retry_count >= 2: return with degradation notice appended to response

#### Step 4.4: Fix FinalError passthrough

**File:** `crates/openfang-runtime/src/agent_loop.rs`
- **At line 377** (after FinalError detection):
- Add `return Err(OpenFangError::ContextOverflow(...))`
- Instead of silently continuing with a corrupt context

#### Step 4.5: Add tests

- Test: ruthless_mode=false passes through bad responses (current behavior)
- Test: ruthless_mode=true retries on TooShort
- Test: ruthless_mode=true stops after 2 retries
- Test: FinalError now returns error instead of continuing

### Validation Checkpoint

```bash
cargo test --workspace -q
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Phase 5: WASM Memory Limits

**Severity:** MEDIUM | **Risk:** MEDIUM | **Estimated changes:** 1 file, ~20 lines
**Dependencies:** None (parallel, but test carefully)

### Problem

`sandbox.rs:39` defines `max_memory_bytes: usize` (default 16MB) but never wires it
to wasmtime's `StoreLimitsBuilder` or `ResourceLimiter`. WASM guests can allocate
up to wasmtime's default (~1GB).

### Implementation Steps

#### Step 5.1: Check wasmtime 41 API for StoreLimitsBuilder

**Pre-step:** Verify the exact API in wasmtime 41:
```bash
cargo doc -p wasmtime --no-deps --open  # or check docs.rs
```

Key types needed:
- `wasmtime::StoreLimitsBuilder` — builder for memory/table limits
- `Store::limiter()` — sets the limiter on the store

#### Step 5.2: Wire memory limits into Store creation

**File:** `crates/openfang-runtime/src/sandbox.rs`
- **At store creation (around line 159-168):**

```rust
// After creating the store:
let limits = wasmtime::StoreLimitsBuilder::new()
    .memory_size(config.max_memory_bytes)
    .build();
store.limiter(|_| &limits);  // or store.data_mut().limiter = limits;
```

Note: The exact API depends on wasmtime 41. `StoreLimitsBuilder` may need the store's
data type to implement `wasmtime::ResourceLimiter`, OR the limiter can be stored
alongside the `GuestState`.

#### Step 5.3: Update GuestState if needed

If wasmtime 41 requires `ResourceLimiter` on the store data type:
- Add a `limiter: StoreLimits` field to `GuestState`
- Implement the store's `limiter()` callback to return `&mut self.limiter`

#### Step 5.4: Add tests

- Test: WASM module that allocates < max_memory_bytes succeeds
- Test: WASM module that allocates > max_memory_bytes gets trapped
- Test: default 16MB limit is applied when no config override

### Validation Checkpoint

```bash
cargo test --workspace -q
cargo clippy --workspace --all-targets -- -D warnings
# If WASM tests exist, verify they still pass with the new limits
```

---

## Execution Order & Dependencies

```
Phase 1 (Input Filtering) ─────┐
Phase 2 (Approval Defaults) ───┤── All independent, can run in parallel
Phase 3 (Secret Redaction) ─────┤
Phase 4 (Quality Gates) ────────┤
Phase 5 (WASM Memory) ─────────┘
```

**Recommended serial order** (if doing one at a time):
1. Phase 1 — Smallest change, highest impact-to-effort ratio
2. Phase 2 — Verify-only, may need zero code changes
3. Phase 3 — Self-contained, no cross-crate deps
4. Phase 4 — Moderate complexity, touches agent loop core
5. Phase 5 — Requires wasmtime API research, highest risk of API mismatch

---

## Final Validation (After All Phases)

```bash
# Full build
cargo build --workspace --lib

# Full test suite (1744+ tests)
cargo test --workspace

# Zero clippy warnings
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Files Modified (Summary)

| File | Phase | Change |
|------|-------|--------|
| `crates/openfang-runtime/src/session_repair.rs` | 1, 3 | Export filter + add redaction |
| `crates/openfang-channels/src/bridge.rs` OR `crates/openfang-kernel/src/kernel.rs` | 1 | Apply input sanitization |
| `crates/openfang-types/src/approval.rs` | 2 | Verify/fix defaults |
| `crates/openfang-runtime/src/agent_loop.rs` | 3, 4 | Add redaction + retry logic |
| `crates/openfang-runtime/src/loop_guard.rs` | 4 | Add ruthless_mode field |
| `crates/openfang-runtime/src/sandbox.rs` | 5 | Wire StoreLimitsBuilder |

---

## Post-Implementation Score Projection

| Category | Before | After | Delta |
|----------|--------|-------|-------|
| Quality gates | 5/10 | 8/10 | +3 |
| Anti-jailbreak | 4.5/10 | 8/10 | +3.5 |
| Crash recovery | 7.5/10 | 8.5/10 | +1 |
| **Overall** | **7.5/10** | **8.5/10** | **+1** |
