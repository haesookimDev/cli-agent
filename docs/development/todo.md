# TODO

향후 개발 계획. 완료 항목은 `docs/development/status.md` 참조.

---

## 우선순위 중간

### C-1. 클러스터 멤버 역할(role) 및 태스크 지정 방식

멤버 등록 시 역할 설명을 함께 저장하고, 태스크 지정을 두 가지 방식으로 지원한다.

#### 백엔드 (`src/orchestrator/cluster.rs`, `src/types.rs`)

- `MemberEntry { orchestrator: Orchestrator, role: String }` 도입 — 현재 `DashMap<String, Orchestrator>` 교체
- `ClusterRunRequest`에 `auto_decompose: bool` 플래그 추가
- **수동 지정** (`auto_decompose: false`, `subtasks` 직접 기입): 현재 동작 유지
- **Planner 자동 분해** (`auto_decompose: true`):
  - 멤버 이름 + role을 컨텍스트로 Planner 오케스트레이터에 주입
  - LLM이 역할별 서브태스크 JSON 반환 → `subtasks` 자동 생성
  - 응답 형식: `[{"member":"...","task":"..."}]`
- `POST /v1/cluster/members` — 런타임 멤버 등록 (`name`, `role` 수신)
- `DELETE /v1/cluster/members/:name` — 런타임 멤버 해제

#### 프론트엔드 (`web/src/`)

- `web/src/lib/types.ts` — `ClusterRunRecord`, `ClusterSubRunEntry`, `ClusterRunStatus` 타입 추가
- `web/src/hooks/use-cluster-run.ts` — `GET /v1/cluster/runs/:id` 1초 폴링 훅
- `web/src/components/cluster-run-card.tsx` — 서브런별 상태 배지 카드 컴포넌트
- `web/src/app/chat/page.tsx` — 클러스터 모드 토글 추가:
  - 토글 OFF: 기존 `POST /v1/runs` 단일 런 흐름
  - 토글 ON: 멤버 선택 + `auto_decompose` 체크박스 → `POST /v1/cluster/runs`
  - 클러스터 런 진행 중: `ClusterRunCard`로 서브런 상태 표시

---

## 장기 아키텍처 개선

### L-1. 중간 상태 체크포인팅
- 크래시 시 인플라이트 노드 상태 손실 방지
- SQLite에 노드 상태 주기적으로 flush

### L-2. 비용 한도 강제 (Cost Budgeting)
- 라우터가 비용 기준으로 모델을 선택하나 한도 초과 시 경고/중단 없음
- 런당 최대 비용 설정 + 초과 시 저비용 모델로 강제 스위칭

### L-3. 크로스-런 의존성 지원
- 각 실행이 완전히 독립적으로, "실행 Y는 실행 X 성공 후 시작" 표현 불가
- 런 간 의존성 DAG 지원

### L-4. 모델 성능 피드백 루프
- 실제 성공/실패 결과가 라우팅 점수에 반영되지 않음
- 모델별 성공률 추적 → 라우팅 가중치 자동 조정

### L-5. 태스크 선점(Preemption) 지원
- 현재 개별 노드 중단 불가, 전체 실행 취소만 가능
- 노드 단위 pause/cancel 신호 추가
