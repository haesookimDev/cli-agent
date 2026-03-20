# Memory & Context System

## 관련 파일

- `src/memory/mod.rs` — MemoryManager (하이브리드 캐시 + DB)
- `src/memory/store.rs` — SQLite 스키마 & 쿼리
- `src/memory/session_log.rs` — JSONL 이벤트 로깅
- `src/context/mod.rs` — ContextManager (토큰 예산 할당)

---

## MemoryManager 아키텍처

```
MemoryManager
  ├── 단기 메모리: DashMap<SessionId, Vec<ShortTermItem>>
  │     - 인메모리, TTL 만료
  │     - 세션 진행 중 빠른 접근
  │
  └── 장기 메모리: SQLite (data/agent.db)
        - session_memories: 세션별 사실 기록
        - knowledge_items: 글로벌 지식 베이스
        - sessions: 세션 메타데이터
        - messages: 채팅 히스토리
        - runs: 실행 기록
```

---

## SQLite 스키마

```sql
-- 세션
sessions (id, created_at, updated_at, metadata)

-- 채팅 메시지
messages (id, session_id, role, content, created_at, run_id)

-- 실행 기록
runs (id, session_id, task, profile, status, created_at, outputs_json, error)

-- 세션 메모리
session_memories (id, session_id, key, value, importance, created_at, expires_at)

-- 글로벌 지식
knowledge_items (id, key, value, category, importance, created_at, updated_at)

-- 실행 이벤트
run_action_events (id, run_id, seq, event_type, timestamp, actor_type, actor_id, payload_json)
```

---

## 메모리 검색 스코어링

메모리 조회 시 관련도 스코어 계산:

```
score = (최신성 0.35) + (중요도 0.35) + (유사도 0.30)

최신성: exp(-decay_rate * age_hours)
중요도: memory.importance (0.0~1.0)
유사도: 키워드 매칭 기반 (현재 임베딩 없음)
```

---

## JSONL 세션 로그

각 세션의 이벤트가 `data/sessions/{session_id}/events.jsonl`에 라인별로 기록:

```jsonl
{"seq":1,"event_type":"RunStarted","timestamp":"...","payload":{...}}
{"seq":2,"event_type":"NodeStarted","timestamp":"...","payload":{"node_id":"plan"}}
{"seq":3,"event_type":"NodeCompleted","timestamp":"...","payload":{"node_id":"plan","output":"..."}}
```

`replay` 서브커맨드로 세션 재현 가능.

---

## ContextManager 토큰 예산 할당

`src/context/mod.rs`의 `ContextManager`는 각 에이전트 호출 전 토큰 예산을 할당:

```
총 예산: AGENT_MAX_CONTEXT_TOKENS (기본 16000)
  ├── 시스템 프롬프트: 고정
  ├── 태스크 설명: 고정
  ├── 의존 노드 출력: 가변 (중요도순 트리밍)
  └── 메모리 컨텍스트: 남은 예산에서 할당
```

**컨텍스트 타입**:
- `session_local`: 현재 세션 내 메모리
- `global`: 글로벌 지식 베이스
- `dependency_outputs`: 선행 노드 결과

---

## 메모리 저장 흐름

```
에이전트 실행 완료
  → AgentOutput.content 추출
  → MemoryManager.store_session_memory(session_id, key, value, importance)
  → 단기: DashMap에 즉시 삽입
  → 장기: SQLite INSERT OR REPLACE
  → SessionLog에 이벤트 기록
```

## 메모리 조회 흐름

```
에이전트 실행 전
  → ContextManager.build_context(session_id, task, token_budget)
  → MemoryManager.retrieve_relevant(session_id, query, limit=20)
       → 단기 DashMap 조회 (TTL 확인)
       → 장기 SQLite 조회
       → 스코어 계산 후 상위 N개 반환
  → 토큰 예산 내에서 컨텍스트 구성
```
