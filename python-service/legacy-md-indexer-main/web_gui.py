# 웹 GUI 모듈
# - Gradio 5.x/6.x 메시지 포맷 대응
# - RAGGenerator 통합 및 실시간 스트리밍
# - 인터랙션 로깅, 등급별 질문 이력

import os
import hashlib
import logging
import gradio as gr
import json
import time
import uuid
from datetime import datetime
from pathlib import Path

from agentic_router import AgenticRouter
from rag_generator import RAGGenerator


# 로거 설정
logger = logging.getLogger("RAG_GUI")
logger.setLevel(logging.INFO)
ch = logging.StreamHandler()
ch.setFormatter(logging.Formatter('[%(levelname)s] %(asctime)s - %(message)s', '%H:%M:%S'))
logger.addHandler(ch)

BASE_DIR = Path(r"C:\Users\Elise\Desktop\2026 Dev\HAI-H\legacy-md-indexer-main")
LOG_FILE_PATH = BASE_DIR / "interaction_logs.json"
USERS_FILE = BASE_DIR / "users.json"


# 계정 관리
def load_users():
    if not USERS_FILE.exists():
        return {}
    try:
        with open(USERS_FILE, 'r', encoding='utf-8') as f:
            return json.load(f)
    except Exception:
        return {}

def save_users(users):
    with open(USERS_FILE, 'w', encoding='utf-8') as f:
        json.dump(users, f, ensure_ascii=False, indent=2)

def hash_password(password: str) -> str:
    return hashlib.sha256(password.encode()).hexdigest()

def register_user(username, password, rank):
    if not username or not password:
        return "아이디와 비밀번호를 입력하세요."
    users = load_users()
    if username in users:
        return "이미 존재하는 아이디입니다."
    users[username] = {
        "password": hash_password(password),
        "rank": rank
    }
    save_users(users)
    return "회원가입 완료. 로그인 탭에서 진행해주세요."

def login_user(username, password):
    users = load_users()
    if username not in users:
        return False, "존재하지 않는 아이디입니다.", ""
    if users[username]["password"] != hash_password(password):
        return False, "비밀번호가 일치하지 않습니다.", ""
    return True, f"{username}님 환영합니다.", users[username]["rank"]


# 텍스트 추출 (Gradio 5.x/6.x 메시지 포맷 대응)
def _extract_text(content) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for item in content:
            if isinstance(item, dict) and 'text' in item:
                parts.append(item['text'])
            elif isinstance(item, str):
                parts.append(item)
        return ''.join(parts) if parts else str(content)
    return str(content)


# 질문 이력 조회
def _load_history_html(user_rank: str, limit: int = 20) -> str:
    if not LOG_FILE_PATH.exists():
        return "<div class='history-empty'>아직 질문 이력이 없습니다.</div>"
    try:
        with open(LOG_FILE_PATH, 'r', encoding='utf-8') as f:
            logs = json.load(f)
    except Exception:
        return "<div class='history-empty'>이력을 불러올 수 없습니다.</div>"

    filtered = [l for l in logs if l.get("user", {}).get("rank") == user_rank]
    filtered = filtered[-limit:][::-1]

    if not filtered:
        return "<div class='history-empty'>아직 질문 이력이 없습니다.</div>"

    html = "<div class='history-list'>"
    for entry in filtered:
        ts = entry.get("timestamp", "")
        user = entry.get("user", {}).get("username", "")
        query = _extract_text(entry.get("query", {}).get("raw_input", ""))
        status = entry.get("status", "")
        total = entry.get("total_duration_s", 0)
        doc_cnt = entry.get("stage1_routing", {}).get("filtered_files_count", 0)

        status_cls = "done" if status == "COMPLETED" else "stopped"
        status_ico = "check-ico" if status == "COMPLETED" else "stop-ico"

        display_q = query if len(query) <= 50 else query[:47] + "..."

        html += f"""<div class='history-item {status_cls}'>
            <div class='history-top'>
                <span class='history-user'>{user}</span>
                <span class='history-time'>{ts}</span>
            </div>
            <div class='history-query'>{display_q}</div>
            <div class='history-meta'>
                <span class='{status_ico}'></span>
                <span>{total:.1f}s &middot; {doc_cnt}건 검색</span>
            </div>
        </div>"""
    html += "</div>"
    return html


# 전역 CSS (Clean Light Theme)
custom_css = """
:root {
    --bg-base:      #f8f9fa;
    --bg-panel:     #ffffff;
    --bg-card:      #fdfdfe;
    --bg-card-h:    #f1f3f5;
    --border:       #e9ecef;
    --border-light: rgba(233, 236, 239, 0.5);
    --accent:       #3b82f6; /* 산뜻한 블루 */
    --accent-hover: #2563eb;
    --accent-dim:   rgba(59, 130, 246, 0.08);
    --green:        #10b981;
    --green-dim:    rgba(16, 185, 129, 0.08);
    --amber:        #f59e0b;
    --amber-dim:    rgba(245, 158, 11, 0.08);
    --red:          #ef4444;
    --red-dim:      rgba(239, 68, 68, 0.08);
    --text-p:       #212529;
    --text-s:       #495057;
    --text-m:       #868e96;
    --radius:       16px;
    --radius-sm:    8px;
    --radius-lg:    24px;
    --shadow-sm:    0 2px 8px rgba(0,0,0,0.04);
    --shadow-md:    0 8px 16px rgba(0,0,0,0.06);
    --shadow-lg:    0 16px 32px rgba(0,0,0,0.08);
    --font-xs:      clamp(0.65rem, 0.6rem + 0.25vw, 0.75rem);
    --font-sm:      clamp(0.75rem, 0.7rem + 0.25vw, 0.8125rem);
    --font-base:    clamp(0.8125rem, 0.75rem + 0.3vw, 0.875rem);
    --font-md:      clamp(0.875rem, 0.8rem + 0.35vw, 0.9375rem);
    --font-lg:      clamp(1.125rem, 1rem + 0.5vw, 1.25rem);
    --font-xl:      clamp(1.25rem, 1.1rem + 0.6vw, 1.5rem);
    --space-xs:     clamp(4px, 0.5vw, 8px);
    --space-sm:     clamp(8px, 1vw, 12px);
    --space-md:     clamp(12px, 1.5vw, 20px);
    --space-lg:     clamp(20px, 2.5vw, 40px);
    --space-xl:     clamp(28px, 3vw, 48px);
}

/* Base Overrides */
.gradio-container, .contain, .app {
    background: var(--bg-base) !important;
    color: var(--text-p) !important;
    line-height: 1.6 !important;
}
.block, .form, .panel {
    background: var(--bg-panel) !important;
    border-color: var(--border) !important;
    box-shadow: var(--shadow-sm) !important;
    border-radius: var(--radius) !important;
}
input, textarea, select {
    background: var(--bg-panel) !important;
    color: var(--text-p) !important;
    border: 1px solid var(--border) !important;
    border-radius: var(--radius-sm) !important;
    font-size: var(--font-base) !important;
    transition: all 0.2s ease !important;
}
input:focus, textarea:focus {
    border-color: var(--accent) !important;
    box-shadow: 0 0 0 3px var(--accent-dim) !important;
    outline: none !important;
}
input::placeholder, textarea::placeholder {
    color: var(--text-m) !important;
}

/* Chatbot */
.chatbot, .chatbot .message-wrap, .chatbot .wrapper {
    background: var(--bg-panel) !important;
    border-radius: var(--radius) !important;
    border: none !important;
}
.chatbot .bot, .chatbot .message.bot {
    background: var(--bg-card) !important;
    color: var(--text-p) !important;
    border-radius: var(--radius) !important;
    padding: var(--space-sm) var(--space-md) !important;
    font-size: var(--font-base) !important;
    border: 1px solid var(--border) !important;
}
.chatbot .user, .chatbot .message.user {
    background: var(--accent) !important;
    color: #ffffff !important;
    border-radius: var(--radius) !important;
    padding: var(--space-sm) var(--space-md) !important;
    font-size: var(--font-base) !important;
}

/* Buttons */
button {
    background: var(--bg-panel) !important;
    color: var(--text-p) !important;
    border: 1px solid var(--border) !important;
    border-radius: var(--radius-sm) !important;
    font-weight: 500 !important;
    transition: all 0.2s ease !important;
}
button:hover {
    border-color: var(--accent) !important;
    color: var(--accent) !important;
}
button.primary, button[variant="primary"] {
    background: var(--accent) !important;
    color: #ffffff !important;
    border-color: var(--accent) !important;
}
button.primary:hover, button[variant="primary"]:hover {
    background: var(--accent-hover) !important;
    color: #ffffff !important;
}
button.stop {
    background: var(--red-dim) !important;
    color: var(--red) !important;
    border-color: rgba(239, 68, 68, 0.3) !important;
}

/* Typography & Layout */
@import url('https://fonts.googleapis.com/css2?family=Pretendard:wght@300;400;500;600;700&display=swap');
* {
    font-family: 'Pretendard', -apple-system, BlinkMacSystemFont, system-ui, sans-serif !important;
}
.gradio-container {
    max-width: 96vw !important;
    padding-left: var(--space-sm) !important;
    padding-right: var(--space-sm) !important;
}

/* Auth Page */
.auth-wrap {
    max-width: min(420px, 92vw);
    margin: clamp(4vh, 8vh, 10vh) auto;
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: var(--space-xl) var(--space-lg);
    box-shadow: var(--shadow-lg);
    box-sizing: border-box;
}
.auth-header {
    text-align: center;
    margin-bottom: var(--space-md);
}
.auth-header h2 {
    color: var(--text-p) !important;
    font-size: var(--font-xl) !important;
    font-weight: 700 !important;
    margin: 0 0 var(--space-xs) 0 !important;
    letter-spacing: -0.5px;
    border: none !important;
}
.auth-header p {
    color: var(--text-m);
    font-size: var(--font-base);
    margin: 0;
}
.auth-msg {
    text-align: center;
    font-size: var(--font-sm);
    margin-top: var(--space-sm);
    font-weight: 500;
}

/* 로그인 탭 디자인 다듬기 */
.auth-wrap .tabs {
    border: none !important;
    background: transparent !important;
}
.auth-wrap .tab-nav {
    border-bottom: 2px solid var(--border-light) !important;
    margin-bottom: var(--space-md) !important;
}
.auth-wrap .tab-nav button {
    font-weight: 600 !important;
    font-size: var(--font-md) !important;
    padding-bottom: var(--space-sm) !important;
    color: var(--text-m) !important;
    border: none !important;
}
.auth-wrap .tab-nav button.selected {
    color: var(--accent) !important;
    border-bottom: 2px solid var(--accent) !important;
}

/* Main Header */
.main-header {
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: var(--space-md) var(--space-md);
    margin-bottom: var(--space-md);
    display: flex;
    justify-content: space-between;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-sm);
}
.main-header .hdr-left h1 { margin: 0; font-size: var(--font-lg) !important; font-weight: 700 !important; color: var(--text-p) !important; }
.main-header .hdr-left .sub { color: var(--text-m); font-size: var(--font-sm); margin-top: var(--space-xs); }
.user-badge { background: var(--accent-dim); color: var(--accent); padding: var(--space-xs) var(--space-sm); border-radius: 20px; font-size: var(--font-sm); font-weight: 600; }
.rank-badge { padding: var(--space-xs) var(--space-sm); border-radius: 20px; font-size: var(--font-xs); font-weight: 600; margin-right: var(--space-xs); }
.rank-badge.hi { background: var(--green-dim); color: var(--green); }
.rank-badge.lo { background: var(--amber-dim); color: var(--amber); }

/* Status Bar */
.status-bar {
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: var(--space-sm) var(--space-md);
    margin-bottom: var(--space-sm);
    display: flex;
    align-items: center;
    font-size: var(--font-sm);
    font-weight: 500;
    background: var(--bg-panel);
}
.status-bar.idle { color: var(--text-m); }
.status-bar.search { background: var(--amber-dim); color: var(--text-p); border-left: 4px solid var(--amber); }
.status-bar.gen { background: var(--green-dim); color: var(--text-p); border-left: 4px solid var(--green); }
.status-bar.done { background: var(--accent-dim); color: var(--accent); border-left: 4px solid var(--accent); }
.status-bar.error { background: var(--red-dim); color: var(--red); border-left: 4px solid var(--red); }

/* Right Panel Elements */
.section-label {
    font-size: var(--font-xs) !important;
    font-weight: 600 !important;
    color: var(--text-s) !important;
    margin: var(--space-md) 0 var(--space-sm) !important;
}
.info-panel { background: var(--bg-base) !important; border: none !important; box-shadow: none !important; }
.summary-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: var(--space-sm); margin-bottom: var(--space-sm); }
.summary-card {
    background: var(--bg-panel); border: 1px solid var(--border);
    border-radius: var(--radius-sm); padding: var(--space-sm) var(--space-xs); text-align: center;
}
.summary-val { font-size: var(--font-lg); font-weight: 700; color: var(--accent); }
.summary-lbl { font-size: var(--font-xs); color: var(--text-m); margin-top: var(--space-xs); }
.info-row { display: flex; justify-content: space-between; padding: var(--space-xs) 0; font-size: var(--font-sm); color: var(--text-s); }

/* Source List */
.source-panel { max-height: clamp(160px, 25vh, 300px); overflow-y: auto; }
.source-item {
    background: var(--bg-panel); border: 1px solid var(--border);
    border-radius: var(--radius-sm); padding: var(--space-sm) var(--space-sm); margin-bottom: var(--space-xs);
    font-size: var(--font-sm); color: var(--text-p);
}
.source-num { color: var(--accent); font-weight: 600; margin-right: var(--space-xs); }
.source-score { font-size: var(--font-xs); background: var(--bg-base); padding: 2px var(--space-xs); border-radius: 4px; margin-left: var(--space-xs); color: var(--text-m); }

/* History List */
.history-list { max-height: clamp(200px, 30vh, 400px); overflow-y: auto; }
.history-item { background: var(--bg-panel); border: 1px solid var(--border); border-radius: var(--radius-sm); padding: var(--space-sm); margin-bottom: var(--space-xs); }
.history-top { display: flex; justify-content: space-between; margin-bottom: var(--space-xs); }
.history-user { font-size: var(--font-xs); font-weight: 600; color: var(--accent); }
.history-time { font-size: var(--font-xs); color: var(--text-m); }
.history-query { font-size: var(--font-sm); color: var(--text-p); margin-bottom: var(--space-xs); }
.history-meta { font-size: var(--font-xs); color: var(--text-m); }

/* Chatbot ↔ Example Row 시각 통합 */
#main-chatbot { border-bottom-left-radius: 0 !important; border-bottom-right-radius: 0 !important; }
#main-chatbot .wrapper, #main-chatbot .chatbot { border-bottom-left-radius: 0 !important; border-bottom-right-radius: 0 !important; }
#example-row {
    background: var(--bg-panel) !important; border: 1px solid var(--border) !important;
    border-top: none !important; border-bottom-left-radius: var(--radius) !important;
    border-bottom-right-radius: var(--radius) !important; margin-top: -1px !important;
    padding: var(--space-sm) var(--space-md) !important; box-shadow: var(--shadow-sm) !important;
    display: flex; gap: var(--space-xs); flex-wrap: wrap; justify-content: center;
}
#example-row button {
    background: var(--bg-card) !important; border: 1px solid var(--border) !important;
    border-radius: 20px !important; padding: var(--space-xs) var(--space-sm) !important;
    font-size: var(--font-sm) !important; color: var(--text-s) !important;
    cursor: pointer !important; transition: all 0.2s ease !important; font-weight: 400 !important;
}
#example-row button:hover {
    background: var(--accent-dim) !important; border-color: var(--accent) !important; color: var(--accent) !important;
}

/* Session Radio Tabs */
#session-radio { background: transparent !important; border: none !important; box-shadow: none !important; padding: 0 !important; }
#session-radio .wrap { display: flex !important; gap: var(--space-xs) !important; flex-wrap: wrap !important; overflow-x: auto !important; }
#session-radio label {
    padding: var(--space-xs) var(--space-sm) !important; font-size: var(--font-sm) !important;
    background: var(--bg-panel) !important; border: 1px solid var(--border) !important;
    border-radius: 20px !important; cursor: pointer !important; color: var(--text-s) !important;
    white-space: nowrap !important; transition: all 0.2s ease !important; font-weight: 500 !important;
}
#session-radio label.selected, #session-radio label:has(input:checked) {
    background: var(--accent) !important; color: #fff !important; border-color: var(--accent) !important;
}
#session-radio input[type="radio"] { display: none !important; }

/* =================== RESPONSIVE BREAKPOINTS =================== */

/* Tablet: <= 1024px */
@media (max-width: 1024px) {
    .gradio-container {
        max-width: 100% !important;
        padding-left: 12px !important;
        padding-right: 12px !important;
    }
    #info-panel-col {
        min-width: 240px !important;
    }
    .main-header {
        padding: 14px 16px;
    }
    .summary-grid {
        gap: 8px;
    }
}

/* Mobile: <= 768px */
@media (max-width: 768px) {
    .gradio-container {
        padding-left: 6px !important;
        padding-right: 6px !important;
    }
    #main-content-row {
        flex-direction: column !important;
    }
    #main-content-row > div {
        width: 100% !important;
        max-width: 100% !important;
        min-width: 0 !important;
        flex: 1 1 100% !important;
    }
    #chat-col { order: 1; }
    #info-panel-col { order: 2; }

    #session-row {
        flex-wrap: wrap !important;
    }
    #session-row > div:first-child {
        flex: 1 1 100% !important;
        max-width: 100% !important;
    }

    .auth-wrap {
        max-width: 100% !important;
        margin: 3vh auto !important;
        padding: 24px 16px !important;
        border-radius: 12px;
    }
    .main-header {
        flex-direction: column;
        align-items: flex-start;
        gap: 10px;
        padding: 12px 14px;
    }
    .source-panel {
        max-height: clamp(120px, 30vh, 240px);
    }
    .history-list {
        max-height: clamp(150px, 30vh, 300px);
    }
    .summary-grid {
        grid-template-columns: repeat(2, 1fr) !important;
    }
    #chat-col .chatbot {
        height: calc(100vh - 280px) !important;
        min-height: 250px !important;
    }
}

/* Small mobile: <= 480px */
@media (max-width: 480px) {
    .auth-wrap {
        padding: 20px 12px !important;
        margin: 2vh 4px !important;
    }
    .main-header .hdr-left h1 {
        font-size: clamp(0.95rem, 4vw, 1.15rem) !important;
    }
    .user-badge, .rank-badge {
        font-size: clamp(0.6rem, 2.5vw, 0.7rem);
        padding: 4px 8px;
    }
    .session-tab {
        padding: 6px 10px;
        font-size: clamp(0.65rem, 2.8vw, 0.75rem);
    }
    .summary-grid {
        grid-template-columns: 1fr !important;
    }
}
"""


# HTML 컴포넌트 렌더링
def _status_idle() -> str:
    return "<div class='status-bar idle'>질문을 입력하면 문서 검색이 시작됩니다.</div>"

def _status_searching() -> str:
    return "<div class='status-bar search'>관련 문서 검색 중...</div>"

def _status_search_done(duration: float, count: int) -> str:
    return f"<div class='status-bar gen'>{count:,}건 문서 발견 ({duration:.1f}s)</div>"

def _status_generating() -> str:
    return "<div class='status-bar gen'>답변 생성 중...</div>"

def _status_complete(route_dur: float, gen_dur: float) -> str:
    total = route_dur + gen_dur
    return f"<div class='status-bar done'>답변 완료 (총 {total:.1f}s)</div>"

def _status_error(msg: str) -> str:
    return f"<div class='status-bar error'>오류: {msg}</div>"

def _build_source_html(sources: list) -> str:
    if not sources:
        return "<div class='source-panel'><div style='color:var(--text-m); font-size:var(--font-sm);'>참조 문서 없음</div></div>"
    html = "<div class='source-panel'>"
    for idx, src in enumerate(sources, 1):
        fpath = src.get("file_path", src) if isinstance(src, dict) else str(src)
        score_val = src.get("score") if isinstance(src, dict) else None
        display_name = Path(fpath).name if "/" in fpath else fpath
        score_html = f"<span class='source-score'>관련도 {score_val:.2f}</span>" if score_val is not None else ""
        html += f"<div class='source-item'><span class='source-num'>[{idx}]</span>{display_name}{score_html}</div>"
    html += "</div>"
    return html

def _stats_empty() -> str:
    return "<div style='color:var(--text-m); font-size:var(--font-sm);'>질문을 입력하면 결과가 표시됩니다.</div>"

def _stats_html(route_dur: float, gen_dur: float, params: dict, doc_count: int) -> str:
    total = route_dur + gen_dur
    years_str  = ", ".join(str(y) for y in params.get("years",  [])) or "전체"
    months_str = ", ".join(f"{m}월" for m in params.get("months", [])) or "전체"
    search_q   = params.get("search_query", "-") or "-"
    return f"""
<div class='summary-grid'>
  <div class='summary-card'><div class='summary-val'>{doc_count:,}</div><div class='summary-lbl'>검색 문서</div></div>
  <div class='summary-card'><div class='summary-val'>{gen_dur:.1f}s</div><div class='summary-lbl'>생성 시간</div></div>
  <div class='summary-card'><div class='summary-val'>{total:.1f}s</div><div class='summary-lbl'>총 소요</div></div>
</div>
<div class='info-row'><span>대상 연도</span><span>{years_str}</span></div>
<div class='info-row'><span>대상 월</span><span>{months_str}</span></div>
<div class='info-row'><span>검색어</span><span>{search_q}</span></div>
"""

def _build_header_html(username: str, user_rank: str) -> str:
    rank_cls = "hi" if user_rank == "hi_rank" else "lo"
    rank_label = "전체 열람" if user_rank == "hi_rank" else "일반 열람"
    return f"""<div class='main-header'>
        <div class='hdr-left'>
            <h1>Legacy Document Intelligence</h1>
            <div class='sub'>AI 기반 레거시 문서 검색 시스템</div>
        </div>
        <div>
            <span class='rank-badge {rank_cls}'>{rank_label}</span>
            <span class='user-badge'>{username}</span>
        </div>
    </div>"""


# 통합 RAG 엔진
class IntegratedRAGEngine:
    def __init__(self):
        self.ready = False
        self.init_error = ""
        catalog_path = Path("./file_catalog.json")

        if not catalog_path.exists():
            self.init_error = "카탈로그 누락: md_catalog_builder.py 실행 필요"
            logger.error(self.init_error)
            return

        try:
            self.router = AgenticRouter(catalog_path=str(catalog_path))
            self.generator = RAGGenerator(target_dir="./processed_md")
            self._init_log_file()
            logger.info("RAG 엔진 초기화 완료")
            self.ready = True
        except Exception as e:
            self.init_error = f"초기화 실패: {e}"
            logger.error(self.init_error)

    def _init_log_file(self):
        if not LOG_FILE_PATH.exists():
            with open(LOG_FILE_PATH, 'w', encoding='utf-8') as f:
                json.dump([], f)

    def _log_interaction(self, *, username: str, user_rank: str, query: str,
                         params: dict, filtered_files: list,
                         bm25_references: list, answer: str,
                         route_duration: float, gen_duration: float,
                         status: str = "COMPLETED"):
        try:
            with open(LOG_FILE_PATH, 'r', encoding='utf-8') as f:
                logs = json.load(f)

            logs.append({
                "timestamp": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
                "status": status,
                "user": {"username": username, "rank": user_rank},
                "query": {
                    "raw_input": query,
                    "extracted_parameters": {
                        "years": params.get("years", []),
                        "months": params.get("months", []),
                        "search_query": params.get("search_query", ""),
                    },
                },
                "stage1_routing": {
                    "duration_s": round(route_duration, 3),
                    "filtered_files_count": len(filtered_files),
                },
                "stage2_generation": {
                    "duration_s": round(gen_duration, 3),
                    "bm25_references": bm25_references,
                },
                "total_duration_s": round(route_duration + gen_duration, 3),
            })

            with open(LOG_FILE_PATH, 'w', encoding='utf-8') as f:
                json.dump(logs, f, ensure_ascii=False, indent=2)
            logger.info("인터랙션 로그 저장 완료")
        except Exception as e:
            logger.error(f"로그 저장 실패: {e}")

    def route_query(self, query: str, user_rank: str = "hi_rank"):
        start_t = time.time()
        route_res = self.router.route_query(query, user_rank=user_rank)
        return route_res, time.time() - start_t


# 세션 유틸리티
def _new_session_id():
    return uuid.uuid4().hex[:8]

def _make_sessions_init():
    sid = _new_session_id()
    return {sid: {"name": "새 대화", "messages": []}}, sid

def _session_display_name(messages: list) -> str:
    for msg in messages:
        if msg.get("role") == "user":
            q = _extract_text(msg["content"])
            return q if len(q) <= 15 else q[:13] + "..."
    return "새 대화"


# UI 빌드 영역
def build_gradio_ui():
    engine = IntegratedRAGEngine()

    example_queries = [
        "2024년 이동장비 주요 정비 내역 자세히 알려줘",
        "교체가 잦은 부품들의 리스트를 보여주세요",
        "원가 구조 요약",
    ]

    with gr.Blocks(css=custom_css, fill_height=True, theme=gr.themes.Default(
        primary_hue="blue",
        neutral_hue="slate",
        font=["Pretendard", "sans-serif"]
    )) as demo:

        user_rank_state = gr.State("low_rank")
        username_state = gr.State("")

        init_sessions, init_active = _make_sessions_init()
        sessions_state = gr.State(init_sessions)
        active_session_state = gr.State(init_active)

        # 로그인 화면
        with gr.Column(visible=True) as auth_view:
            with gr.Column(elem_classes="auth-wrap"):
                gr.HTML("""
                    <div class='auth-header'>
                        <h2>문서 검색 시스템</h2>
                        <p>계정 정보를 입력하여 시작하세요.</p>
                    </div>
                """)

                # with gr.Tabs():
                #     with gr.Tab("로그인"):
                login_user_input = gr.Textbox(label="아이디", placeholder="아이디 입력")
                login_pw_input = gr.Textbox(label="비밀번호", type="password", placeholder="비밀번호 입력")
                login_btn = gr.Button("로그인", variant="primary")
                login_msg = gr.HTML("<div class='auth-msg'></div>")

                    # with gr.Tab("회원가입"):
                    #     reg_user_input = gr.Textbox(label="아이디", placeholder="사용할 아이디")
                    #     reg_pw_input = gr.Textbox(label="비밀번호", type="password", placeholder="비밀번호 설정")
                    #     reg_rank_input = gr.Radio(["일반 열람 (low_rank)", "전체 열람 (hi_rank)"], label="권한", value="일반 열람 (low_rank)")
                    #     reg_btn = gr.Button("회원가입")
                    #     reg_msg = gr.HTML("<div class='auth-msg'></div>")

        # 메인 화면
        with gr.Column(visible=False, elem_id="main-view") as main_view:
            header_display = gr.HTML("")

            with gr.Row(elem_id="session-row"):
                with gr.Column(scale=8):
                    session_radio = gr.Radio(
                        choices=[(v["name"], k) for k, v in init_sessions.items()],
                        value=init_active, label="", container=False,
                        elem_id="session-radio", interactive=True
                    )
                with gr.Column(scale=1, min_width=50):
                    new_session_btn = gr.Button("+ 새 대화", size="sm")
                with gr.Column(scale=1, min_width=50):
                    del_session_btn = gr.Button("삭제", size="sm", variant="stop")

            with gr.Row(elem_id="main-content-row"):
                with gr.Column(scale=7, elem_id="chat-col"):
                    gr.HTML("""<div style='background:var(--amber-dim); border:1px solid rgba(245,158,11,0.25);
                        border-radius:var(--radius-sm); padding:var(--space-sm) var(--space-md);
                        font-size:var(--font-sm); color:var(--text-s); display:flex; align-items:center; gap:var(--space-sm);'>
                        <span style='font-size:1.1em;'>⚠</span>
                        <span>본 시스템은 <strong>프로토타입</strong>으로 불안정할 수 있습니다.
                        질의 후 답변까지 약 <strong>1~2분</strong> 소요됩니다.</span>
                    </div>""")
                    chatbot = gr.Chatbot(
                        height="calc(100vh - 320px)", show_label=False,
                        elem_id="main-chatbot",
                        placeholder="<div style='text-align:center;color:var(--text-m);padding:40px 20px;'>"
                                    "<div style='font-size:var(--font-lg);font-weight:600;margin-bottom:8px;'>무엇을 검색할까요?</div>"
                                    "<div style='font-size:var(--font-sm);'>예시를 클릭하거나 직접 질문을 입력하세요.</div></div>"
                    )
                    with gr.Row(elem_classes="example-row", elem_id="example-row"):
                        ex_btns = [gr.Button(q, size="sm") for q in example_queries]
                    with gr.Row():
                        msg_input = gr.Textbox(show_label=False, placeholder="질문 입력...", container=False, scale=7)
                        submit_btn = gr.Button("검색", variant="primary", scale=1)
                        stop_btn   = gr.Button("중지", variant="stop", scale=1)

                with gr.Column(scale=3, elem_id="info-panel-col", elem_classes="info-panel"):
                    status_display = gr.HTML(value=_status_idle())

                    gr.HTML("<div class='section-label'>검색 결과 요약</div>")
                    stats_display = gr.HTML(value=_stats_empty())

                    gr.HTML("<div class='section-label'>참조 문서 목록</div>")
                    source_display = gr.HTML(value="<div class='source-panel'><div style='color:var(--text-m); font-size:var(--font-sm);'>질문을 입력하면 참조 문서가 표시됩니다.</div></div>")

                    gr.HTML("<div class='section-label'>최근 질문 이력</div>")
                    history_display = gr.HTML(value="<div class='history-empty'>질문을 입력하면 이력이 표시됩니다.</div>")

        # 상태 관리
        msg_query_state = gr.State()
        route_res_state = gr.State()
        route_dur_state = gr.State()

        def _radio_choices(sessions):
            return [(v["name"], k) for k, v in sessions.items()]

        # 이벤트
        def do_login(user, pw):
            success, msg, rank = login_user(user, pw)
            if success:
                header = _build_header_html(user, rank)
                history = _load_history_html(rank)
                sessions, active = _make_sessions_init()
                return (gr.update(visible=False), gr.update(visible=True), rank, user,
                        f"<div class='auth-msg' style='color:var(--green);'>{msg}</div>",
                        header, history, sessions, active,
                        gr.update(choices=_radio_choices(sessions), value=active), [])
            return (gr.update(visible=True), gr.update(visible=False), "low_rank", "",
                    f"<div class='auth-msg' style='color:var(--red);'>{msg}</div>",
                    "", "", gr.update(), gr.update(), gr.update(), gr.update())

        login_btn.click(do_login, inputs=[login_user_input, login_pw_input],
                        outputs=[auth_view, main_view, user_rank_state, username_state, login_msg,
                                 header_display, history_display, sessions_state, active_session_state,
                                 session_radio, chatbot])

        # def do_register(user, pw, rank_label):
        #     rank = "hi_rank" if "hi_rank" in rank_label else "low_rank"
        #     msg = register_user(user, pw, rank)
        #     color = "var(--green)" if "완료" in msg else "var(--red)"
        #     return f"<div class='auth-msg' style='color:{color};'>{msg}</div>"

        # reg_btn.click(do_register, inputs=[reg_user_input, reg_pw_input, reg_rank_input], outputs=[reg_msg])

        def create_new_session(sessions, active_id, current_chat):
            if active_id in sessions:
                sessions[active_id]["messages"] = current_chat or []
                sessions[active_id]["name"] = _session_display_name(current_chat or [])
            new_id = _new_session_id()
            sessions[new_id] = {"name": "새 대화", "messages": []}
            return (sessions, new_id, [],
                    gr.update(choices=_radio_choices(sessions), value=new_id),
                    _status_idle(), _stats_empty(), "<div class='source-panel'><div style='color:var(--text-m); font-size:var(--font-sm);'>질문을 입력하면 참조 문서가 표시됩니다.</div></div>")

        new_session_btn.click(create_new_session, inputs=[sessions_state, active_session_state, chatbot],
                              outputs=[sessions_state, active_session_state, chatbot, session_radio,
                                       status_display, stats_display, source_display])

        def delete_current_session(sessions, active_id):
            if len(sessions) <= 1:
                ns, na = _make_sessions_init()
                return (ns, na, [],
                        gr.update(choices=_radio_choices(ns), value=na),
                        _status_idle(), _stats_empty(), "<div class='source-panel'><div style='color:var(--text-m); font-size:var(--font-sm);'>질문을 입력하면 참조 문서가 표시됩니다.</div></div>")
            sessions.pop(active_id, None)
            new_active = list(sessions.keys())[-1]
            new_chat = sessions[new_active].get("messages", [])
            return (sessions, new_active, new_chat,
                    gr.update(choices=_radio_choices(sessions), value=new_active),
                    _status_idle(), _stats_empty(), "<div class='source-panel'><div style='color:var(--text-m); font-size:var(--font-sm);'>질문을 입력하면 참조 문서가 표시됩니다.</div></div>")

        del_session_btn.click(delete_current_session, inputs=[sessions_state, active_session_state],
                              outputs=[sessions_state, active_session_state, chatbot, session_radio,
                                       status_display, stats_display, source_display])

        def switch_session(selected_id, sessions, active_id, current_chat):
            if selected_id == active_id:
                return gr.update(), sessions, active_id, gr.update()
            if active_id in sessions:
                sessions[active_id]["messages"] = current_chat or []
                sessions[active_id]["name"] = _session_display_name(current_chat or [])
            new_chat = sessions.get(selected_id, {}).get("messages", [])
            return new_chat, sessions, selected_id, gr.update(choices=_radio_choices(sessions), value=selected_id)

        session_radio.change(switch_session, inputs=[session_radio, sessions_state, active_session_state, chatbot],
                             outputs=[chatbot, sessions_state, active_session_state, session_radio])

        def user_interaction(user_message, history):
            history = history or []
            history.append({"role": "user", "content": user_message})
            return "", history

        def bot_interaction_route(history, user_rank):
            if not history or history[-1]["role"] != "user":
                yield history, _status_idle(), {}, 0, ""
                return
            if not engine.ready:
                history.append({"role": "assistant", "content": "시스템 오류: 관리자 문의 필요"})
                yield history, _status_error("엔진 미초기화"), {}, 0, ""
                return

            query = _extract_text(history[-1]["content"])
            history.append({"role": "assistant", "content": "문서 검색 중..."})
            yield history, _status_searching(), {}, 0, query

            route_res, route_dur = engine.route_query(query, user_rank)
            yield history, _status_search_done(route_dur, len(route_res.get("target_files", []))), route_res, route_dur, query

        def bot_interaction_generate(history, route_res, route_duration, query, username, user_rank, sessions, active_id):
            if not history or history[-1]["role"] != "assistant":
                yield history, _build_source_html([]), _stats_empty(), _status_idle(), gr.update(), sessions, active_id, gr.update()
                return

            chat_hist = [m for m in history[:-1] if m.get("content") and "검색 중" not in m["content"]]
            start_gen_t = time.time()
            full_ans = ""
            sources = []

            yield history, _build_source_html([]), _stats_empty(), _status_generating(), gr.update(), sessions, active_id, gr.update()

            try:
                for chunk in engine.generator.generate_stream(
                    query, route_res["target_files"], search_query=route_res.get("search_query", query),
                    catalog=engine.router.catalog, params=route_res.get("parameters", {}), chat_history=chat_hist
                ):
                    full_ans = chunk["answer"]
                    sources = chunk["references"]
                    history[-1]["content"] = full_ans
                    yield history, _build_source_html(sources), _stats_empty(), _status_generating(), gr.update(), sessions, active_id, gr.update()

                gen_dur = time.time() - start_gen_t
                engine._log_interaction(
                    username=username, user_rank=user_rank, query=query, params=route_res.get("parameters", {}),
                    filtered_files=route_res.get("target_files", []), bm25_references=sources, answer=full_ans,
                    route_duration=route_duration, gen_duration=gen_dur, status="COMPLETED"
                )

                if active_id in sessions:
                    sessions[active_id]["messages"] = history
                    sessions[active_id]["name"] = _session_display_name(history)

                yield (history, _build_source_html(sources), _stats_html(route_duration, gen_dur, route_res.get("parameters", {}), len(route_res.get("target_files", []))),
                       _status_complete(route_duration, gen_dur), _load_history_html(user_rank), sessions, active_id,
                       gr.update(choices=_radio_choices(sessions), value=active_id))

            except Exception as e:
                logger.warning(f"추론 중단/오류 발생: {e}")
                history[-1]["content"] = full_ans + "\n\n[출력 중단됨]"
                engine._log_interaction(
                    username=username, user_rank=user_rank, query=query, params=route_res.get("parameters", {}),
                    filtered_files=route_res.get("target_files", []), bm25_references=sources, answer=history[-1]["content"],
                    route_duration=route_duration, gen_duration=time.time() - start_gen_t, status="STOPPED"
                )
                if active_id in sessions:
                    sessions[active_id]["messages"] = history
                    sessions[active_id]["name"] = _session_display_name(history)

                yield (history, _build_source_html(sources), _stats_empty(), _status_error(str(e)), _load_history_html(user_rank),
                       sessions, active_id, gr.update(choices=_radio_choices(sessions), value=active_id))

        _inputs = [chatbot, route_res_state, route_dur_state, msg_query_state, username_state, user_rank_state, sessions_state, active_session_state]
        _outputs = [chatbot, source_display, stats_display, status_display, history_display, sessions_state, active_session_state, session_radio]

        submit_ev = submit_btn.click(user_interaction, [msg_input, chatbot], [msg_input, chatbot], queue=False)\
            .then(bot_interaction_route, [chatbot, user_rank_state], [chatbot, status_display, route_res_state, route_dur_state, msg_query_state], queue=True)\
            .then(bot_interaction_generate, _inputs, _outputs, queue=True)

        input_ev = msg_input.submit(user_interaction, [msg_input, chatbot], [msg_input, chatbot], queue=False)\
            .then(bot_interaction_route, [chatbot, user_rank_state], [chatbot, status_display, route_res_state, route_dur_state, msg_query_state], queue=True)\
            .then(bot_interaction_generate, _inputs, _outputs, queue=True)

        for btn in ex_btns:
            btn.click(fn=lambda q: q, inputs=[btn], outputs=[msg_input], queue=False)

        stop_btn.click(fn=None, inputs=None, outputs=None, cancels=[submit_ev, input_ev])

    return demo

if __name__ == "__main__":
    logger.info("웹 서버 시작")
    app = build_gradio_ui()
    app.launch(server_name="0.0.0.0", server_port=7860, share=True, allowed_paths=["."])