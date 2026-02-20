# Rust 멀티 에이전트 오케스트레이터 고도화: 6단계 진화

## 개요

Rust 기반 멀티 에이전트 오케스트레이터 시스템에 6가지 핵심 기능을 추가했다. 채팅 인터페이스, 모델 관리 설정, 크론잡 스케줄링, 에이전트 역할 세분화, 완료 검증, 메모리/캐싱 고도화까지 — 단순 DAG 실행기에서 지능적 에이전트 오케스트레이터로의 진화를 기록한다.

## 기술 구현

### Phase 1: 채팅 인터페이스 + 활성 세션 표시

에이전트와의 상호작용을 실시간 채팅 형태로 보여주는 UI를 구현했다.

**백엔드**: `GET /v1/runs/active`와 `GET /v1/sessions/:id/messages` 두 엔드포인트를 추가. Orchestrator의 DashMap에서 Running/Queued 상태의 run을 필터링하고, 세션 메시지와 에이전트 출력을 시간순으로 병합한다.

**프론트엔드**: ChatBubble 컴포넌트(역할별 스타일링), AgentThinking 애니메이션, useActiveRuns 폴링 훅(3초 간격), SSE 기반 실시간 이벤트 수신.

```typescript
// useActiveRuns.ts — 활성 런 폴링
const { activeRuns, count } = useActiveRuns();
// Nav에서 count > 0일 때 오렌지 배지 표시
```

### Phase 2: 설정 페이지 + 모델 관리

웹 UI에서 모델 활성화/비활성화, 선호 모델 지정이 가능하도록 했다.

**핵심 결정**: `parking_lot::RwLock` 대신 `std::sync::Mutex`를 선택. parking_lot이 의존성에 없었고, preferred_model 접근 빈도가 낮아 std::sync으로 충분했다.

```rust
// ModelRouter에 preferred_model 보너스 점수 부여
if preferred.as_deref() == Some(model.model_id.as_str()) {
    score += 0.15;
}
```

Settings 페이지는 프로바이더별 토글 버튼과 모델 테이블(Quality, Latency, Cost, Context 컬럼)로 구성.

### Phase 3: 크론잡 스케줄링

저장된 워크플로우를 cron 표현식으로 반복 실행하는 스케줄러를 구현했다.

**아키텍처**: `CronScheduler`가 60초마다 `list_due_schedules(now)`로 실행 대기 스케줄을 조회하고, `execute_workflow()`로 워크플로우를 트리거한다. `cron` 크레이트(0.13)로 6-field cron 표현식 파싱.

```rust
// scheduler.rs — 핵심 루프
loop {
    sleep(Duration::from_secs(60)).await;
    let due = memory.list_due_schedules(Utc::now()).await?;
    for schedule in due {
        orchestrator.execute_workflow(&schedule.workflow_id, ...).await?;
        memory.update_schedule_last_run(schedule.id, now, compute_next_run(&schedule.cron_expr)).await?;
    }
}
```

**주의점**: `main.rs`에서 `memory`가 Orchestrator 생성 시 move되므로, `orchestrator.memory().clone()`으로 접근해야 했다.

### Phase 4: 에이전트 역할 세분화 + 지능적 선택

10개 에이전트 역할(기존 6 + Analyzer, Reviewer, Scheduler, ConfigManager)과 태스크 분류 기반 적응형 그래프 빌더를 구현했다.

**`classify_task()`**: 키워드 기반 휴리스틱으로 6가지 TaskType 분류 (LLM 호출 없음, 동기 함수).

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

**핵심**: SimpleQuery는 plan→summarize 2노드, CodeGeneration/Complex는 6노드 풀 그래프. 이전에는 모든 요청이 6노드 그래프를 타서 단순 질문도 과도한 LLM 호출이 발생했다.

**기존 테스트 호환**: `ExecutionPolicy`에 `Option<String>` 필드가 있어 `Copy` 불가. `..default_policy` 패턴 대신 `Self::default_policy()` 팩토리 함수로 해결.

### Phase 5: 오케스트레이터 완료 검증

실행 완료 후 Reviewer 에이전트가 결과를 검증하는 루프를 추가했다.

```rust
// execute_run 내부, 그래프 실행 성공 후
let verified = self.verify_completion(run_id, session_id, &req.task, &results).await;
```

Reviewer는 "COMPLETE" 또는 "INCOMPLETE: <이유>"를 출력. SSE 이벤트(`verification_started`, `verification_complete`)로 프론트엔드에 자동 표시.

**실패 시 안전장치**: Reviewer 에이전트가 사용 불가하면 (모델 연결 실패 등) 실행을 차단하지 않고 COMPLETE로 가정.

### Phase 6: 메모리/컨텍스트/캐싱 고도화

세 가지 개선:

1. **LLM 응답 캐싱** (router/mod.rs): 프롬프트 해시 기반, 프로필별 TTL (Planning 5분, Extraction 15분), 최대 500항목, 자동 만료 정리.

2. **트라이그램 유사도** (memory/mod.rs): 기존 키워드 Jaccard에 3-gram 유사도 추가 (60% 키워드 + 40% 트라이그램).

3. **크로스세션 지식** (knowledge_base 테이블): topic/content/importance 기반 저장, access_count 자동 증가.

## 도전과 해결

### 문제 1: Axum GET 핸들러에서 body 추출기 충돌

GET 요청 핸들러에 `body: Bytes`와 `Query` 추출기를 함께 사용하면 Axum의 `Handler` trait가 만족되지 않는다. GET에는 body가 없으므로, auth 검증에 `&[]`를 전달하고 body 파라미터를 제거.

### 문제 2: ExecutionPolicy의 move semantics

`ExecutionPolicy`에 `Option<String>` (fallback_node)이 있어 `Copy` 불가. `..default_policy` struct update 문법은 첫 사용 후 move. 팩토리 함수 `Self::default_policy()`로 매번 새 인스턴스 생성.

### 문제 3: memory Arc move 후 borrow

`Orchestrator::new()`에 `memory`를 넘기면 소유권이 이전됨. 이후 `CronScheduler::new(memory.clone(), ...)`는 컴파일 에러. `orchestrator.memory().clone()`으로 접근.

## 핵심 교훈

1. **적응형 그래프가 핵심이다**: 모든 요청에 동일 그래프를 사용하면 단순 질문도 6개 LLM 호출을 소비. 태스크 분류 → 맞춤 그래프로 비용과 지연 시간 절감.

2. **검증 루프는 graceful해야 한다**: Reviewer 실패 시 전체 파이프라인을 블로킹하면 안 됨. 장애 허용 설계(fail-open).

3. **캐싱 TTL은 프로필마다 달라야 한다**: Planning 결과(전략적)는 빠르게 만료, Extraction 결과(사실 기반)는 오래 유지.

4. **Rust의 소유권 모델은 설계를 강제한다**: Arc, Clone, 팩토리 패턴 — 컴파일러가 올바른 아키텍처로 유도.
