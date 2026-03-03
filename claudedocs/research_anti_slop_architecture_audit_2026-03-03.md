# OpenFang Anti-Slop Architecture Audit
**Date:** 2026-03-03
**Branch:** `custom/integrations`
**Scope:** Full codebase audit against agentic engineering best practices

---

## Executive Summary

OpenFang's `custom/integrations` branch implements a **sophisticated Host-Guest architecture** that aligns strongly with the blueprint principles from the research material. The kernel acts as a secure Host managing scheduling, API keys, and memory, while agents run as capability-restricted Guests in WASM sandboxes with deny-by-default permissions.

**Overall alignment score: 8/10** — The architecture is production-grade with excellent isolation, capability enforcement, and loop detection. Key gaps exist in output quality gates (detection without rejection), LLM jailbreak defense, and automatic crash recovery.

---

## Source Material: Key Principles

### From "How Top Engineers Stop AI Agents From Writing Slop" (Jaymin West)
1. **Never fix bad output** — reset and retry from scratch
2. **Quality gates & hooks** — strict linting, type-checking before advancing
3. **Anti-mocking** — test real integrations, not mocks
4. **Isolation & hard blocks** — per-agent worktrees, no destructive actions
5. **One agent, one task, one prompt** — focused agents are correct agents

### From "How Hackers Are Using AI" (Steve Sims)
1. **Chatbot social engineering** — attackers trick AI bots into leaking secrets
2. **Swarm attacks** — micro-agents attacking around the clock
3. **Defense falling behind** — less time to patch as attack surface grows

### From "AI Is A Massive Problem" (Palisade Research)
1. **AIs are grown, not programmed** — we cannot fully predict behavior
2. **Nudge, not control** — guardrails are probabilistic, not deterministic
3. **Recursive self-improvement risk** — AI improving AI

### Blueprint Architecture
- **Host** (Rust kernel): manages memory, scheduling, API keys
- **Guests** (WASM agents): run in strict sandboxes
- **Bridge**: host exposes capabilities with permission checks
- **Hard blocks**: system-level prevention of destructive actions
- **Ruthless task destruction**: drop bad output, spawn fresh

---

## Alignment Analysis

### 1. Host-Guest Architecture

| Blueprint Requirement | OpenFang Implementation | Status |
|---|---|---|
| Host manages memory | `OpenFangKernel` owns `MemorySubstrate` (SQLite-backed structured, semantic, session stores) | **IMPLEMENTED** |
| Host manages scheduling | `ScheduleMode` (Reactive/Periodic/Proactive/Continuous) + cron system | **IMPLEMENTED** |
| Host manages API keys | `api_key_env` in agent TOML, keys resolved at runtime, never passed to guests | **IMPLEMENTED** |
| Guests in WASM sandboxes | `wasmtime v41` with fuel metering (1M instructions), epoch deadlines, linear memory isolation | **IMPLEMENTED** |
| Bridge with permission checks | JSON RPC `host_call` dispatch, 22 capability types, deny-by-default | **IMPLEMENTED** |
| Guests cannot access internet/FS directly | All I/O goes through host functions in `host_functions.rs` | **IMPLEMENTED** |

**Verdict:** The Host-Guest separation is **fully implemented** with proper privilege mediation.

---

### 2. Isolation & Hard Blocks

| Principle | Implementation | Status |
|---|---|---|
| **Per-agent memory isolation** | Each agent has separate `Session`, `structured_get(agent_id, key)` scoping, `memory_read: ["self.*"]` default | **IMPLEMENTED** |
| **WASM linear memory isolation** | Separate linear memory per guest, bounds checking on every pointer | **IMPLEMENTED** |
| **Filesystem sandboxing** | Workspace sandbox, path traversal blocked (".."), symlink escape prevention, `allowed_paths` whitelist | **IMPLEMENTED** |
| **Process isolation** | `env_clear()` on all children, only 17 safe env vars re-added, credential leakage blocked | **IMPLEMENTED** |
| **Docker sandbox** (optional) | Container-level isolation with capability dropping, network isolation | **IMPLEMENTED** |
| **Hard block: `rm -rf /`** | Destructive pattern detection in `subprocess_sandbox.rs` | **IMPLEMENTED** |
| **Hard block: fork bombs** | Pattern matching blocks `:(){ :|:& };:` and variants | **IMPLEMENTED** |
| **Hard block: daemon self-kill** | PID check prevents killing own process | **IMPLEMENTED** |
| **Hard block: disk format** | `mkfs`, `dd if=/dev/zero` blocked | **IMPLEMENTED** |
| **SSRF protection** | Private IPs blocked (127.0.0.1, 169.254.169.254, metadata.*) | **IMPLEMENTED** |
| **Privilege non-escalation** | `validate_capability_inheritance()` — child caps must be subset of parent | **IMPLEMENTED** |

**Verdict:** Isolation is **comprehensive and multi-layered** (WASM + filesystem + process + network + Docker).

---

### 3. Capability-Based Permission Model

OpenFang implements **22 distinct capability types** with pattern matching:

```
FileRead/Write(glob)    NetConnect/Listen(host:port)    ShellExec(cmd_pattern)
ToolInvoke(tool_id)     LlmQuery(model)                 LlmMaxTokens(budget)
AgentSpawn/Message/Kill  MemoryRead/Write(scope)         EnvRead(var)
OfpDiscover/Connect/Advertise    EconSpend/Earn/Transfer
```

**Three enforcement modes:**
- **Deny**: Block all shell execution
- **Allowlist** (default): Only `safe_bins` + `allowed_commands`
- **Full**: All commands (dev only, still respects deny patterns)

**Agent modes provide tier-based access:**

| Mode | Analogy | Access |
|---|---|---|
| **Observe** | Scout | Read-only, no tool execution |
| **Assist** | Worker | Read-only tools (`file_read`, `memory_recall`, `web_fetch`) |
| **Full** | Admin | All granted tools |

**Verdict:** The capability model **exceeds the blueprint** with fine-grained, pattern-matched, hierarchical permissions.

---

### 4. Anti-Slop Mechanisms

#### a. Loop Detection (Excellent - 9/10)

The `LoopGuard` implements multi-layer loop detection:
- **Identical call blocking**: SHA-256 hash of (tool_name + params), warn at 3, block at 5
- **Outcome-aware detection**: Hashes (tool + params + result), blocks identical outcomes
- **Ping-pong detection**: Catches A-B-A-B alternating patterns after 3 repeats
- **Circuit breaker**: Hard stop after 30 total tool calls
- **Poll relaxation**: Known-good polling commands get 3x relaxed thresholds

#### b. Quality Gates (Partial - 5/10)

`detect_response_issues()` catches:
- Too-short responses (>100 char input, <30 char output)
- Unaddressed tool errors
- Fallback template detection

**GAP:** Quality issues are **logged but not rejected**. The response is still returned to the user. This violates "never fix bad output, reset and retry."

#### c. Context Overflow Recovery (Good - 7/10)

4-stage progressive pipeline:
1. Auto-compaction (keep last 10 messages) at 70-90% full
2. Aggressive trim (keep last 4) if still >90%
3. Truncate historical tool results to 2K chars
4. Return FinalError, suggest `/reset`

#### d. Taint Tracking (Partial - 5/10)

Lattice-based taint labels: `ExternalNetwork`, `UserInput`, `Pii`, `Secret`, `UntrustedAgent`

Taint sinks block flows:
- `shell_exec` blocks: ExternalNetwork, UntrustedAgent, UserInput
- `net_fetch` blocks: Secret, Pii
- `agent_message` blocks: Secret

**GAP:** Heuristic pattern matching only (8 hardcoded patterns like `curl`, `eval`). No real information flow propagation.

#### e. Approval Gates (Present - 6/10)

Dangerous tools require human approval: `shell_exec` (Critical), `file_write`/`file_delete` (High), `web_fetch` (Medium).

**GAP:** Approval can be disabled. Autonomous agents auto-approve (bad for safety in multi-tenant).

---

### 5. "One Agent, One Task, One Prompt"

| Aspect | Implementation | Status |
|---|---|---|
| Single system prompt per agent | `system_prompt` in agent.toml, injected into every LLM call | **IMPLEMENTED** |
| Isolated sessions | Per-agent conversation history, fresh context for delegation | **IMPLEMENTED** |
| Clear capability boundaries | `ManifestCapabilities` define what agent *can* do | **IMPLEMENTED** |
| Focused agent types | Coder, Researcher, Assistant, Debugger, etc. with role-specific prompts | **IMPLEMENTED** |
| Subagent depth limits | `subagent_max_depth: 10`, leaf agents can't spawn further | **IMPLEMENTED** |
| Subagent concurrency limits | `subagent_max_concurrent: 5` | **IMPLEMENTED** |

**Verdict:** The single-purpose agent model is **well-implemented** with proper depth/concurrency controls.

---

### 6. Extensibility Model

| Component | Purpose | Implementation |
|---|---|---|
| **Hands** | Packaged autonomous agents (clip processor, email monitor) | `openfang-hands` crate with requirement checking, settings resolution, dashboard metrics |
| **Skills** | Pluggable tool bundles (Python, WASM, Node, PromptOnly) | `openfang-skills` with provenance tracking (Native, Bundled, ClawHub, Local) |
| **Extensions** | MCP server integrations (GitHub, Slack, Google Calendar) | `openfang-extensions` with 25 bundled templates, credential vault (AES-256-GCM) |
| **WASM Plugins** | Custom guest modules | `wasmtime` sandbox with host function bridge |

**Verdict:** Extensibility is **excellent** — new functionality can be added as WASM files, skills, or MCP servers without modifying the core.

---

### 7. Security Against AI Threats

#### Chatbot Social Engineering Defense

| Defense | Status | Notes |
|---|---|---|
| Tool execution gating | **IMPLEMENTED** | Capability checks before every tool call |
| Taint tracking on inputs | **PARTIAL** | Heuristic patterns, not semantic analysis |
| Rate limiting | **IMPLEMENTED** | GCRA algorithm, 500 tokens/min/IP |
| Secret exfiltration prevention | **PARTIAL** | Blocks secrets in URLs, not in conversation text |
| Prompt injection filters | **MISSING** | No system prompt injection detection |
| Jailbreak detection | **MISSING** | No semantic analysis of "breaking character" |
| Response content filtering | **MISSING** | No post-generation content safety checks |

#### Swarm Attack Defense

| Defense | Status | Notes |
|---|---|---|
| Per-IP rate limiting | **IMPLEMENTED** | 429 responses with retry-after |
| Per-agent resource quotas | **IMPLEMENTED** | `ResourceQuota` (memory, CPU, tokens, cost) |
| Spawn chain limits | **IMPLEMENTED** | Depth + concurrency caps on subagents |
| WebSocket rate limiting | **IMPLEMENTED** | 10 messages/60s per connection |

---

## Gap Analysis: What's Missing

### Critical Gaps

1. **Output Quality Rejection** (Anti-Slop Priority #1)
   - Quality issues are detected but responses are still returned
   - Need: Hard rejection + automatic retry with modified prompt
   - Blueprint principle: "Never fix bad output, reset and retry"

2. **LLM Jailbreak/Prompt Injection Defense**
   - No detection of adversarial prompts in user input
   - No semantic analysis of responses for "breaking character"
   - Attackers can social-engineer the bot through Discord/channels

3. **Automatic Crash Recovery**
   - Supervisor detects crashes but doesn't auto-respawn
   - Requires external orchestration (systemd) for restart
   - Need: Built-in watchdog with state rollback

### Moderate Gaps

4. **Taint Propagation**
   - Current: 8 hardcoded heuristic patterns
   - Need: Real information flow tracking through tool result chains

5. **Shared Memory Namespace**
   - KV store uses agent_id scoping but namespace isn't fully isolated
   - One agent could theoretically read another's values if patterns match

6. **WASM Memory Quota Enforcement**
   - Config field exists but not enforced via wasmtime's `MemoryType::resource_limits`

### Minor Gaps

7. **Tool policy bypass at WASM layer** — WASM guest calling `host_call("tool_invoke")` could bypass agent_loop policy
8. **Per-agent rate limiting** — current rate limiting is per-IP only
9. **Audit trail enforcement** — approval decisions not durably logged

---

## Recommendations (For Human Decision)

### Immediate (Strengthen existing anti-slop)

1. **Add quality gate rejection mode**: When `detect_response_issues()` fires, optionally reject + retry with an appended "Your previous response was rejected because: {reason}. Try again." This is a config toggle per-agent.

2. **Add prompt injection detection**: Scan user inputs for known injection patterns ("ignore previous instructions", "system prompt:", etc.) before passing to LLM. Log and optionally block.

3. **Enforce WASM memory limits**: Wire up `wasmtime::MemoryType` resource limits using the existing config field.

### Medium-term (Security hardening)

4. **Implement response content filtering**: Post-generation check for leaked secrets, PII, or "breaking character" indicators.

5. **Add per-agent rate limiting**: Extend GCRA to track per-agent-id, not just per-IP.

6. **Upgrade taint tracking**: Move from heuristic patterns to real information flow labels that propagate through tool chains.

7. **Self-healing daemon loop**: Add a watchdog thread that monitors the main process and restarts on crash with checkpoint state rollback.

### Long-term (Blueprint completion)

8. **Formal capability verification**: Static analysis that proves an agent TOML config cannot access resources outside its declared scope.

9. **Semantic proof-checking**: Use a secondary LLM to validate primary agent output against task requirements before accepting.

10. **Network namespace isolation**: Per-agent Linux network namespaces or cgroup-based network policies for true network isolation.

---

## Architecture Scorecard

| Principle | Score | Notes |
|---|---|---|
| Host-Guest separation | **10/10** | Kernel as host, WASM agents as guests, JSON RPC bridge |
| Capability-based permissions | **9/10** | 22 types, pattern matching, inheritance validation |
| Agent isolation (memory) | **8/10** | Per-agent sessions + scoped KV, minor namespace gap |
| Agent isolation (process) | **9/10** | WASM + subprocess + Docker, env_clear, path traversal blocked |
| Hard blocks on destructive actions | **9/10** | rm -rf, fork bombs, self-kill, disk format all blocked |
| Loop detection & circuit breaking | **9/10** | Multi-layer (hash, outcome, ping-pong, circuit breaker) |
| Quality gates (output validation) | **5/10** | Detection exists, rejection missing |
| "One agent, one task, one prompt" | **9/10** | Single prompt, isolated sessions, depth limits |
| Anti-social-engineering defense | **4/10** | Tool gating good, prompt injection defense missing |
| Extensibility | **9/10** | Skills + Hands + Extensions + WASM plugins |
| Crash recovery | **6/10** | Supervisor detects, external restart needed |
| Test quality (anti-mocking) | **8/10** | 770+ tests, mocks only at I/O boundaries, real LLM integration tests |

**Overall: 8/10** — Production-grade architecture with strong fundamentals. Primary gaps are in output rejection (anti-slop) and adversarial input defense (anti-jailbreak).

---

## Key File References

| File | Purpose |
|---|---|
| `crates/openfang-runtime/src/sandbox.rs` | WASM engine, guest ABI, fuel metering |
| `crates/openfang-runtime/src/host_functions.rs` | Capability dispatch, SSRF protection |
| `crates/openfang-types/src/capability.rs` | 22 capability types, matching rules, inheritance |
| `crates/openfang-runtime/src/workspace_sandbox.rs` | Filesystem isolation, symlink escape prevention |
| `crates/openfang-runtime/src/subprocess_sandbox.rs` | Env sandboxing, destructive pattern blocking |
| `crates/openfang-runtime/src/tool_policy.rs` | Deny-wins rules, depth restrictions |
| `crates/openfang-runtime/src/agent_loop.rs` | Quality detection, loop guard, context overflow |
| `crates/openfang-kernel/src/kernel.rs` | Core Host, capability enforcement, agent lifecycle |
| `crates/openfang-hands/src/lib.rs` | Autonomous agent packages |
| `crates/openfang-skills/src/lib.rs` | Pluggable tool bundles |
| `crates/openfang-extensions/src/lib.rs` | MCP server integrations |

---

*This report is research only. Implementation decisions should be made by the project owner.*
