# Memory System

## 관련 파일

- `src/memory/mod.rs` — MemoryManager
- `src/memory/store.rs` — SQLite 백엔드
- `src/memory/session_log.rs` — JSONL 세션 로그

---

## 초기화 흐름

```
Orchestrator 생성 시:
  MemoryManager::new(database_url, data_dir)
    → sqlx::SqlitePool 연결
    → 스키마 마이그레이션 실행 (sessions, messages, runs, memories, knowledge_items, run_action_events 테이블)
    → DashMap 초기화 (빈 단기 캐시)
    → SessionLog 디렉토리 확인 (data/sessions/)
```

---

## 메모리 저장 흐름

### 단기 메모리 저장

```
MemoryManager.store_short_term(session_id, key, value, importance, ttl)
  → DashMap.entry(session_id).or_default().push(ShortTermItem {
      key, value, importance,
      created_at: Instant::now(),
      expires_at: Instant::now() + ttl,
    })
```

### 장기 메모리 저장

```
MemoryManager.store_session_memory(session_id, key, value, importance)
  → SQLite: INSERT OR REPLACE INTO session_memories
      (id, session_id, key, value, importance, created_at)

MemoryManager.store_knowledge(key, value, category, importance)
  → SQLite: INSERT OR REPLACE INTO knowledge_items
      (id, key, value, category, importance, created_at, updated_at)
```

---

## 메모리 조회 흐름

```
MemoryManager.retrieve_relevant(session_id, query, limit)
  ↓
1. DashMap에서 session_id 캐시 조회
   → 만료된 항목 제거 (expires_at < now)

2. SQLite에서 session_memories 조회 (session_id 조건)

3. SQLite에서 knowledge_items 조회 (전체)

4. 각 항목 스코어 계산:
   score = (recency_score * 0.35) + (importance * 0.35) + (similarity_score * 0.30)

   recency_score = exp(-0.1 * age_in_hours)
   similarity_score = keyword_overlap(query, item.key + item.value)

5. 스코어 내림차순 정렬 → 상위 limit개 반환
```

---

## 스코어링 상세

```
최신성 (Recency, 35%):
  age = now - created_at  (시간 단위)
  recency = exp(-0.1 * age)

  예: 1시간 전 → 0.905, 24시간 전 → 0.091, 72시간 전 → 0.0007

중요도 (Importance, 35%):
  item.importance (0.0 ~ 1.0, 저장 시 설정)
  에이전트 판단에 따라 0.3 ~ 0.9 범위로 설정

유사도 (Similarity, 30%):
  query 토큰과 (key + value) 토큰의 Jaccard 유사도
  = |query_tokens ∩ item_tokens| / |query_tokens ∪ item_tokens|
```

---

## JSONL 세션 로그

**파일**: `src/memory/session_log.rs`

각 런의 이벤트가 `data/sessions/{session_id}/run_{run_id}.jsonl`에 기록:

```jsonl
{"seq":1,"event_type":"RunStarted","timestamp":"2024-01-01T00:00:00Z","run_id":"...","task":"..."}
{"seq":2,"event_type":"NodeStarted","timestamp":"...","node_id":"plan","role":"Planner"}
{"seq":3,"event_type":"NodeCompleted","timestamp":"...","node_id":"plan","output":"계획: ...","tokens":150}
{"seq":4,"event_type":"NodeStarted","timestamp":"...","node_id":"extract","role":"Extractor"}
...
{"seq":N,"event_type":"RunCompleted","timestamp":"...","status":"Succeeded"}
```

`cargo run -- replay <session_id>` 명령으로 세션 재현 가능.

---

## SQLite 스키마 요약

```sql
-- 세션 메타데이터
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  metadata TEXT          -- JSON
);

-- 채팅 메시지
CREATE TABLE messages (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  role TEXT NOT NULL,    -- "user" | "agent"
  content TEXT NOT NULL,
  created_at TEXT NOT NULL,
  run_id TEXT            -- 연결된 run_id (선택적)
);

-- 실행 기록
CREATE TABLE runs (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  task TEXT NOT NULL,
  profile TEXT,
  status TEXT NOT NULL,  -- "Queued" | "Running" | "Succeeded" | "Failed" | "Cancelled"
  created_at TEXT NOT NULL,
  outputs_json TEXT,     -- Vec<AgentExecutionRecord>
  error TEXT
);

-- 실행 이벤트 (SSE, 트레이스용)
CREATE TABLE run_action_events (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  event_type TEXT NOT NULL,
  timestamp TEXT NOT NULL,
  actor_type TEXT,
  actor_id TEXT,
  payload_json TEXT
);

-- 세션 메모리
CREATE TABLE session_memories (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  key TEXT NOT NULL,
  value TEXT NOT NULL,
  importance REAL DEFAULT 0.5,
  created_at TEXT NOT NULL,
  expires_at TEXT
);

-- 글로벌 지식 베이스
CREATE TABLE knowledge_items (
  id TEXT PRIMARY KEY,
  key TEXT NOT NULL UNIQUE,
  value TEXT NOT NULL,
  category TEXT,
  importance REAL DEFAULT 0.5,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
```
