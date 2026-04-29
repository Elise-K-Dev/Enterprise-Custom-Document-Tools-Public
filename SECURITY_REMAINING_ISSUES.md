# 남은 보안/운영 리스크

이 문서는 내부 토큰 검증, 등록 계정 기반 다운로드 토큰, PDF 렌더러 비root 실행, `/document/fill-fields` 응답 축소를 적용한 뒤에도 남는 리스크를 정리한 것이다.

## 높은 우선순위

1. `scripts/sync_openwebui_runtime.sh`와 `open-webui/.env.openwebui`에는 아직 초기 계정 비밀번호 기본값이 남아 있다. 운영 환경에서는 기본값을 제거하고, 최초 실행 시 외부에서 전달된 강한 비밀번호만 받도록 바꿔야 한다.
2. 서비스 포트 `8001`, `8002`, `8003`은 여전히 compose에서 호스트로 노출된다. 내부 토큰 검증으로 임의 호출은 막았지만, 로컬 전용이면 `127.0.0.1:8001:8001`처럼 loopback 바인딩으로 줄이는 것이 더 안전하다.
3. Open WebUI에 저장되는 도구 서버 헤더에는 `PORT_PROJECT_INTERNAL_TOKEN` 값이 들어간다. Open WebUI 관리자 권한을 가진 사람이 이 값을 볼 수 있으므로, 관리자 계정 보호와 토큰 주기적 회전 절차가 필요하다.
4. 다운로드 토큰은 메모리 기반이며 만료 전에는 링크를 가진 사람이 재사용할 수 있다. 현재 TTL 기본값은 3600초다. 더 엄격한 통제가 필요하면 1회성 토큰 또는 Open WebUI 인증 프록시 방식으로 바꿔야 한다.
5. 서비스 간 통신과 다운로드 URL은 HTTP다. 로컬 단일 장비가 아니라 LAN/운영망에서 쓴다면 TLS 또는 리버스 프록시 인증 계층이 필요하다.

## 중간 우선순위

1. CORS 정책은 아직 넓게 열려 있다. 내부 토큰이 없는 브라우저 요청은 실패하지만, 운영 배포에서는 Open WebUI origin만 허용하도록 줄이는 것이 좋다.
2. Chromium sandbox는 기본적으로 켜지도록 바꿨지만, 실제 Docker 런타임에서 커널 sandbox 지원 여부를 확인해야 한다. 문제가 생길 때만 `CHROMIUM_DISABLE_SANDBOX=true`를 임시로 사용한다.
3. Docker 이미지에는 Python 검색 데이터와 Rust DB 입력 데이터를 직접 넣지 않도록 줄였지만, 호스트의 `processed_md`, `file_catalog.json`, `users.json`, `rust-service/DB/input` 자체는 민감 데이터다. 백업/권한/폐기 정책이 필요하다.
4. `open-webui/openwebui-*.json` 수동 import 파일에는 실제 내부 토큰이 들어 있지 않다. 현재 보안 구성은 `scripts/sync_openwebui_runtime.sh` 실행을 전제로 한다.
5. `.openwebui-build`, `rust-service/target`, `processed_md` 같은 로컬 산출물이 수 GB 단위로 남아 있다. 주기적인 정리 작업을 두는 것이 좋다.

## 검증 필요

1. 현재 작업 셸에는 `cargo`와 `docker`가 없어 Rust 테스트와 `docker compose config`를 실행하지 못했다.
2. Python 문법 검사는 통과했다.
3. 실제 확인할 항목:
   - Open WebUI 도구 호출에 `X-Port-Project-Internal-Token`과 사용자 헤더가 함께 전달되는지
   - 등록 계정이 아닌 직접 호출이 `403` 또는 `401`로 차단되는지
   - 생성된 다운로드 링크가 TTL 만료 후 실패하는지
   - Chromium PDF 렌더링이 `--no-sandbox` 없이 정상 동작하는지
