- 이 프로젝트를 개발하기 위한 중요한 사항들 중 영구적으로 보존해야할 부분은 이곳에 추가해라
- 구조화되고 식별이 쉽게 작성해랴한다.

## Runtime Invariants

### Commit Discipline
- 큰 변경을 한 번에 커밋하지 말고, 논리적으로 독립된 작업 단위로 나눠 소커밋한다.
- 코드 수정 중에는 단계별 검증(관련 테스트/빌드/타입체크) 통과 후 즉시 커밋한다.
- 한 커밋은 가능한 한 하나의 목적만 포함해야 하며, 무관한 파일 변경을 섞지 않는다.
- 커밋 메시지는 작업 의도와 영향 범위를 명확하게 작성한다.

### Session ID Consistency
- `submit_run`에서 생성/확정한 `session_id`는 `execute_run`까지 동일하게 전달되어야 한다.
- 새 세션 실행에서 `session_id`를 다시 생성하면 사용자 메시지가 다른 세션으로 저장되어 새로고침 시 대화가 사라지는 문제가 발생한다.

### Streaming Event Ordering
- `node_token_chunk` 이벤트는 저장 순서가 응답 텍스트 순서와 동일해야 한다.
- 토큰마다 `tokio::spawn`으로 DB insert를 분기하면 순서가 뒤섞일 수 있으므로 직렬 큐(worker)로 저장한다.

### UTF-8 Safe SSE Parsing
- 모델 스트림 파싱은 바이트 버퍼 기반으로 처리해 UTF-8 경계를 보존해야 한다.
- 청크 단위 `from_utf8_lossy` 누적은 한글/다국어에서 `�` 문자를 유발할 수 있다.

### Memory Model
- 세션 메모리는 `memory_items` 테이블을 사용하며 `session_id`로 격리된다.
- 전역(공통) 메모리는 `knowledge_base` 테이블을 사용하며 모든 세션에서 공유된다.
- 실행 컨텍스트 구성 시 세션 메모리(`retrieve`)와 전역 메모리(`search_knowledge`)를 함께 주입해야 한다.
- 메모리 관리 API 경로:
  - 세션 메모리 조회/추가: `/v1/memory/sessions/:session_id/items`
  - 세션 메모리 수정: `/v1/memory/items/:memory_id`
  - 전역 메모리 조회/추가: `/v1/memory/global/items`
  - 전역 메모리 수정: `/v1/memory/global/items/:knowledge_id`
