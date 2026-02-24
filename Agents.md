- 이 프로젝트를 개발하기 위한 중요한 사항들 중 영구적으로 보존해야할 부분은 이곳에 추가해라
- 구조화되고 식별이 쉽게 작성해랴한다.

## Runtime Invariants

### Session ID Consistency
- `submit_run`에서 생성/확정한 `session_id`는 `execute_run`까지 동일하게 전달되어야 한다.
- 새 세션 실행에서 `session_id`를 다시 생성하면 사용자 메시지가 다른 세션으로 저장되어 새로고침 시 대화가 사라지는 문제가 발생한다.

### Streaming Event Ordering
- `node_token_chunk` 이벤트는 저장 순서가 응답 텍스트 순서와 동일해야 한다.
- 토큰마다 `tokio::spawn`으로 DB insert를 분기하면 순서가 뒤섞일 수 있으므로 직렬 큐(worker)로 저장한다.

### UTF-8 Safe SSE Parsing
- 모델 스트림 파싱은 바이트 버퍼 기반으로 처리해 UTF-8 경계를 보존해야 한다.
- 청크 단위 `from_utf8_lossy` 누적은 한글/다국어에서 `�` 문자를 유발할 수 있다.
