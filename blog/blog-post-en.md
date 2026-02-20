# Evolving a Rust Multi-Agent Orchestrator: A 6-Phase Journey

## Overview

Six core capabilities were added to a Rust-based multi-agent orchestrator system: a chat interface, model management settings, cron job scheduling, granular agent roles, completion verification, and memory/caching improvements. This documents the evolution from a simple DAG executor to an intelligent agent orchestrator.

## Technical Implementation

### Phase 1: Chat Interface + Active Session Display

A UI was built to show agent interactions in a real-time chat format.

**Backend**: Two new endpoints — `GET /v1/runs/active` and `GET /v1/sessions/:id/messages`. Running/Queued runs are filtered from the Orchestrator's DashMap, and session messages are merged with agent outputs in chronological order.

**Frontend**: ChatBubble component (role-based styling), AgentThinking animation, useActiveRuns polling hook (3-second interval), SSE-based real-time event streaming.

```typescript
// useActiveRuns.ts — active run polling
const { activeRuns, count } = useActiveRuns();
// Show orange badge in nav when count > 0
```

### Phase 2: Settings Page + Model Management

The web UI was extended to allow enabling/disabling models and designating a preferred model.

**Key Decision**: Chose `std::sync::Mutex` over `parking_lot::RwLock`. parking_lot wasn't in the dependency tree, and the access frequency for preferred_model was low enough that std::sync sufficed.

```rust
// Bonus score for preferred_model in ModelRouter
if preferred.as_deref() == Some(model.model_id.as_str()) {
    score += 0.15;
}
```

The Settings page features per-provider toggle buttons and a model table with Quality, Latency, Cost, and Context columns.

### Phase 3: Cron Job Scheduling

A scheduler was implemented to repeatedly execute saved workflows via cron expressions.

**Architecture**: `CronScheduler` queries `list_due_schedules(now)` every 60 seconds and triggers workflows via `execute_workflow()`. The `cron` crate (0.13) handles 6-field cron expression parsing.

```rust
// scheduler.rs — core loop
loop {
    sleep(Duration::from_secs(60)).await;
    let due = memory.list_due_schedules(Utc::now()).await?;
    for schedule in due {
        orchestrator.execute_workflow(&schedule.workflow_id, ...).await?;
        memory.update_schedule_last_run(schedule.id, now, compute_next_run(&schedule.cron_expr)).await?;
    }
}
```

**Gotcha**: In `main.rs`, `memory` is moved when creating the Orchestrator, so it must be accessed via `orchestrator.memory().clone()`.

### Phase 4: Granular Agent Roles + Intelligent Selection

Ten agent roles (6 existing + Analyzer, Reviewer, Scheduler, ConfigManager) and a task-classification-based adaptive graph builder were implemented.

**`classify_task()`**: Keyword-based heuristic classifying into 6 TaskTypes (no LLM call, synchronous function).

```rust
fn classify_task(task: &str) -> TaskType {
    let lower = task.to_lowercase();
    // Configuration → config keywords
    // ToolOperation → MCP keywords
    // CodeGeneration → code keywords
    // Analysis → analysis keywords
    // SimpleQuery → short (≤8 words), no conjunctions
    // Complex → fallback
}
```

**Key insight**: SimpleQuery uses a 2-node graph (plan → summarize), while CodeGeneration/Complex uses a full 6-node graph. Previously, every request went through the 6-node graph, causing excessive LLM calls even for simple questions.

**Test compatibility**: `ExecutionPolicy` has an `Option<String>` field making it non-`Copy`. The `..default_policy` struct update pattern fails after first use. Solved with a `Self::default_policy()` factory function.

### Phase 5: Orchestrator Completion Verification

A verification loop was added where a Reviewer agent validates results after execution completes.

```rust
// Inside execute_run, after successful graph execution
let verified = self.verify_completion(run_id, session_id, &req.task, &results).await;
```

The Reviewer outputs either "COMPLETE" or "INCOMPLETE: <reason>". SSE events (`verification_started`, `verification_complete`) automatically surface in the frontend.

**Safety net**: If the Reviewer agent is unavailable (e.g., model connection failure), execution is not blocked — COMPLETE is assumed (fail-open design).

### Phase 6: Memory/Context/Caching Improvements

Three enhancements:

1. **LLM Response Caching** (router/mod.rs): Prompt-hash-based caching with profile-specific TTLs (Planning: 5 min, Extraction: 15 min), max 500 entries, automatic expiration cleanup.

2. **Trigram Similarity** (memory/mod.rs): Added 3-gram similarity on top of keyword Jaccard (60% keyword + 40% trigram weighting).

3. **Cross-Session Knowledge** (knowledge_base table): Stored by topic/content/importance, with automatic access_count increment.

## Challenges & Solutions

### Problem 1: Axum GET Handler Body Extractor Conflict

Using both `body: Bytes` and `Query` extractors in a GET handler fails to satisfy Axum's `Handler` trait. Since GET requests have no body, the fix was to pass `&[]` for auth verification and remove the body parameter entirely.

### Problem 2: ExecutionPolicy Move Semantics

`ExecutionPolicy` contains `Option<String>` (fallback_node), making it non-`Copy`. The `..default_policy` struct update syntax moves on first use. Solved by creating a `Self::default_policy()` factory function that produces a fresh instance each time.

### Problem 3: Memory Arc Borrow After Move

Passing `memory` into `Orchestrator::new()` transfers ownership. A subsequent `CronScheduler::new(memory.clone(), ...)` is a compile error. Fixed by accessing it through `orchestrator.memory().clone()`.

## Key Takeaways

1. **Adaptive graphs are essential**: Using the same graph for every request wastes 6 LLM calls on simple questions. Task classification with tailored graphs cuts cost and latency.

2. **Verification loops must be graceful**: A failing Reviewer should never block the entire pipeline. Fail-open design is critical.

3. **Cache TTLs should vary by profile**: Planning results (strategic) should expire quickly; Extraction results (factual) can persist longer.

4. **Rust's ownership model enforces architecture**: Arc, Clone, factory patterns — the compiler guides you toward correct design.
