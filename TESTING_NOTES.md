# 개발 테스트 노트

## 2026-04-25 재고/문서 보조 모델 통합 도구 테스트

### 테스트 목적

- 전 기능 도구 호출 체이닝 검증
- 서버 런타임 환경의 파일 시스템 권한 확인

### 테스트 시나리오

1. 부족 재고 식별 및 상세 컨텍스트 추출
2. 재고 리포트 및 구매 품의 패키지 파일 생성
3. 분석 보고서 PDF 렌더링 및 채팅 기록 XLSX 내보내기

### 도구별 검증 결과

| 도구 | 결과 | 메모 |
| --- | --- | --- |
| `list_shortage_items` | 정상 | 부족 품목 및 미확인 품목 데이터 식별 |
| `get_item_document_context` | 정상 | 품목별 seed 필드 및 가이드 필드 추출 |
| `render_markdown_pdf` | 정상 | 구조화된 보고서 작성 및 PDF 다운로드 URL 생성 |
| `render_chat_xlsx` | 정상 | 대화 이력 기반 Excel 파일 생성 및 URL 반환 |
| `export_inventory_report` | 실패 | HTTP 400, `os error 13: Permission denied` |
| `generate_purchase_document_package` | 실패 | HTTP 400, `os error 13: Permission denied` |

### 주요 발견 사항

- 모델 로직은 사용자의 의도에 맞는 도구 선택 및 복합 체이닝을 안정적으로 수행했다.
- 파일 생성 도구에서 에러가 발생해도 분석 보고서 PDF 생성 같은 후속 단계를 수행하는 복구 흐름을 확인했다.
- Rust 기반 파일 생성 도구는 `/app/DB/output` 하위 경로에 직접 쓰기 때문에, 호스트 바인드 마운트 권한이 맞지 않으면 `Permission denied`가 발생한다.
- 실제 확인 시 `rust-service/DB/output` 하위 일부 날짜별/보고서 디렉터리가 `nfsnobody:nfsnobody` 소유 및 `755` 권한으로 남아 있었다.

### 조치 사항

- `scripts/start_openwebui_with_vllm.sh`에 Docker root 컨테이너를 이용한 바인드 마운트 권한 보정 절차를 추가했다.
- 시작 시 다음 경로를 uid/gid `1000:1000` 및 group-writable 권한으로 정리한다.
  - `rust-service/DB/output`
  - `python-service/output`
  - `markdown-pdf-service/output`

### 재검증 필요

- 권한 보정 후 `export_inventory_report` 재실행
- 권한 보정 후 `generate_purchase_document_package` 재실행
- 생성된 다운로드 링크가 Open WebUI에서 정상 접근되는지 확인
