"""
Phase 3 RAG Generator (BM25 + Kiwi Tokenizer)
- Kiwi 형태소 분석기 기반 BM25 본문 검색
- Stream response 처리 (vLLM 호환)
- Context Window(128k) 대응 및 사전 Truncate
"""
import logging
import requests
import configparser
import hashlib
import json
import os
import threading
import time
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path
from typing import List, Dict, Any, Generator, Union
from rank_bm25 import BM25Okapi
from kiwipiepy import Kiwi

logger = logging.getLogger("RAG_GEN")
logger.setLevel(logging.INFO)
ch = logging.StreamHandler()
ch.setFormatter(logging.Formatter('[%(levelname)s] %(asctime)s - %(message)s', '%H:%M:%S'))
logger.addHandler(ch)

_WORKER_KIWI = None
_BM25_INDEX_LOCK = threading.RLock()
_BM25_INDEX_CACHE: Dict[str, Dict[str, Any]] = {}


def _init_tokenizer_worker() -> None:
    global _WORKER_KIWI
    _WORKER_KIWI = Kiwi()


def _tokenize_text_worker(text: str) -> List[str]:
    global _WORKER_KIWI
    if _WORKER_KIWI is None:
        _WORKER_KIWI = Kiwi()
    if not text.strip():
        return []
    tokens = _WORKER_KIWI.tokenize(text)
    return [t.form for t in tokens if not t.tag.startswith('J') and not t.tag.startswith('S')]


def _normalize_doc_path(path: str) -> str:
    return str(path).replace("\\", "/").lstrip("/")


class RAGGenerator:
    def __init__(self, target_dir: str):
        self.target_dir = Path(target_dir)
        
        config = configparser.ConfigParser()
        config.read('setting.conf')
        self.model_id = config['DEFAULT'].get('MODEL_ID', 'gemma-4-31b-it')
        # Chat completions 엔드포인트 사용
        self.api_url = config['DEFAULT'].get('API_URL', 'http://hai-server:8000/v1/chat/completions')

        self.req_timeout = 600
        self.max_char_limit = 160000 
        
        self.top_k = 5
        default_workers = min(4, os.cpu_count() or 1)
        self.tokenizer_workers = max(1, int(os.getenv("RAG_TOKENIZER_WORKERS", str(default_workers))))
        self.parallel_tokenize_min_docs = max(1, int(os.getenv("RAG_PARALLEL_TOKENIZE_MIN_DOCS", "16")))
        cache_file = os.getenv("RAG_TOKEN_CACHE_FILE", "/app/output/cache/bm25_tokens.json")
        self.token_cache_file = Path(cache_file)
        self.token_cache: Dict[str, Any] | None = None

        logger.info("Kiwi 형태소 분석기 초기화")
        self.kiwi = Kiwi()

    def _tokenize(self, text: str) -> List[str]:
        if not text.strip():
            return []
        # 형태소 분석 후 조사(J) 및 기호(S) 제외
        tokens = self.kiwi.tokenize(text)
        return [t.form for t in tokens if not t.tag.startswith('J') and not t.tag.startswith('S')]

    def _tokenize_docs(self, docs: List[str]) -> List[List[str]]:
        if self.tokenizer_workers <= 1 or len(docs) < self.parallel_tokenize_min_docs:
            return [self._tokenize(doc) for doc in docs]

        workers = min(self.tokenizer_workers, len(docs))
        logger.info(f"Kiwi 토큰화 병렬 처리: workers={workers}, docs={len(docs)}")
        with ProcessPoolExecutor(max_workers=workers, initializer=_init_tokenizer_worker) as executor:
            return list(executor.map(_tokenize_text_worker, docs, chunksize=1))

    def _load_token_cache(self) -> Dict[str, Any]:
        if self.token_cache is not None:
            return self.token_cache

        self.token_cache = {"version": 1, "docs": {}}
        if not self.token_cache_file.exists():
            return self.token_cache

        try:
            loaded = json.loads(self.token_cache_file.read_text(encoding="utf-8"))
            if loaded.get("version") == 1 and isinstance(loaded.get("docs"), dict):
                self.token_cache = loaded
                logger.info(f"BM25 토큰 캐시 로드: {len(self.token_cache['docs'])}개")
        except Exception as exc:
            logger.warning(f"BM25 토큰 캐시 로드 실패. 새로 생성합니다: {exc}")
        return self.token_cache

    def _save_token_cache(self) -> None:
        if self.token_cache is None:
            return
        try:
            self.token_cache_file.parent.mkdir(parents=True, exist_ok=True)
            tmp_path = self.token_cache_file.with_suffix(self.token_cache_file.suffix + ".tmp")
            tmp_path.write_text(
                json.dumps(self.token_cache, ensure_ascii=False, separators=(",", ":")),
                encoding="utf-8",
            )
            tmp_path.replace(self.token_cache_file)
        except Exception as exc:
            logger.warning(f"BM25 토큰 캐시 저장 실패: {exc}")

    def _catalog_meta(self, catalog: Dict | None, path: str) -> Dict:
        if not catalog:
            return {}
        return catalog.get(path) or catalog.get(path.replace("/", "\\")) or {}

    def _load_tokenized_docs(self, file_paths: List[str]) -> tuple[List[str], List[List[str]], tuple]:
        cache = self._load_token_cache()
        docs_cache = cache.setdefault("docs", {})
        valid_paths: List[str] = []
        tokenized_docs: List[List[str] | None] = []
        fingerprint_parts = []
        uncached_docs: List[str] = []
        uncached_slots: List[int] = []
        uncached_meta: List[tuple[str, int, int]] = []
        cache_hits = 0
        cache_changed = False

        for raw_path in file_paths:
            path = _normalize_doc_path(raw_path)
            fpath = self.target_dir / path
            if not fpath.exists():
                logger.warning(f"File not found: {path}")
                continue

            stat = fpath.stat()
            mtime_ns = int(stat.st_mtime_ns)
            size = int(stat.st_size)
            fingerprint_parts.append((path, mtime_ns, size))
            entry = docs_cache.get(path)
            if (
                entry
                and int(entry.get("mtime_ns", -1)) == mtime_ns
                and int(entry.get("size", -1)) == size
                and isinstance(entry.get("tokens"), list)
            ):
                valid_paths.append(path)
                tokenized_docs.append(entry["tokens"])
                cache_hits += 1
                continue

            content = fpath.read_text(encoding='utf-8')
            if not content.strip():
                continue
            valid_paths.append(path)
            tokenized_docs.append(None)
            uncached_slots.append(len(tokenized_docs) - 1)
            uncached_docs.append(content)
            uncached_meta.append((path, mtime_ns, size))

        if uncached_docs:
            new_tokens = self._tokenize_docs(uncached_docs)
            for slot, tokens, (path, mtime_ns, size) in zip(uncached_slots, new_tokens, uncached_meta):
                tokenized_docs[slot] = tokens
                docs_cache[path] = {"mtime_ns": mtime_ns, "size": size, "tokens": tokens}
                cache_changed = True

        if cache_hits or uncached_docs:
            logger.info(f"BM25 토큰 캐시: hit={cache_hits}, miss={len(uncached_docs)}")
        if cache_changed:
            self._save_token_cache()

        return valid_paths, [tokens for tokens in tokenized_docs if tokens is not None], tuple(fingerprint_parts)

    def _scan_doc_fingerprint(self, file_paths: List[str]) -> tuple[List[str], tuple]:
        valid_paths: List[str] = []
        fingerprint_parts = []

        for raw_path in file_paths:
            path = _normalize_doc_path(raw_path)
            fpath = self.target_dir / path
            if not fpath.exists():
                logger.warning(f"File not found: {path}")
                continue

            stat = fpath.stat()
            valid_paths.append(path)
            fingerprint_parts.append((path, int(stat.st_mtime_ns), int(stat.st_size)))

        return valid_paths, tuple(fingerprint_parts)

    def _bm25_cache_key(self, fingerprint: tuple) -> str:
        payload = json.dumps(
            [str(self.target_dir.resolve()), fingerprint],
            ensure_ascii=False,
            separators=(",", ":"),
        )
        return hashlib.sha256(payload.encode("utf-8")).hexdigest()

    def _get_bm25_index(self, file_paths: List[str]) -> tuple[List[str], BM25Okapi | None]:
        scanned_paths, fingerprint = self._scan_doc_fingerprint(file_paths)
        if not scanned_paths:
            return [], None

        cache_key = self._bm25_cache_key(fingerprint)
        with _BM25_INDEX_LOCK:
            entry = _BM25_INDEX_CACHE.get(cache_key)
            if entry and entry.get("fingerprint") == fingerprint:
                entry["last_used"] = time.time()
                logger.info(f"BM25 메모리 인덱스 재사용: docs={len(entry['valid_paths'])}")
                return entry["valid_paths"], entry["bm25"]

        valid_paths, tokenized_docs, loaded_fingerprint = self._load_tokenized_docs(file_paths)
        if not tokenized_docs:
            return [], None
        if loaded_fingerprint != fingerprint:
            fingerprint = loaded_fingerprint
            cache_key = self._bm25_cache_key(fingerprint)

        logger.info(f"BM25 메모리 인덱스 생성: docs={len(tokenized_docs)}")
        bm25 = BM25Okapi(tokenized_docs)

        max_entries = max(1, int(os.getenv("RAG_BM25_MEMORY_CACHE_MAX_ENTRIES", "8")))
        with _BM25_INDEX_LOCK:
            _BM25_INDEX_CACHE[cache_key] = {
                "fingerprint": fingerprint,
                "valid_paths": valid_paths,
                "bm25": bm25,
                "last_used": time.time(),
            }
            if len(_BM25_INDEX_CACHE) > max_entries:
                oldest_key = min(
                    _BM25_INDEX_CACHE,
                    key=lambda key: _BM25_INDEX_CACHE[key].get("last_used", 0),
                )
                if oldest_key != cache_key:
                    _BM25_INDEX_CACHE.pop(oldest_key, None)

        return valid_paths, bm25

    def _retrieve_bm25(self, query: str, file_paths: List[str],
                       catalog: Dict = None, params: Dict = None) -> List[Dict[str, Any]]:
        valid_paths, bm25 = self._get_bm25_index(file_paths)

        if bm25 is None:
            return []

        logger.info(f"{len(valid_paths)}개 문서 BM25 스코어 계산")

        tokenized_query = self._tokenize(query)
        scores = bm25.get_scores(tokenized_query)

        # 날짜 매칭 문서 스코어 부스트
        if catalog and params:
            target_years = set(params.get("years") or [])
            target_months = set(params.get("months") or [])
            if target_years or target_months:
                for i, path in enumerate(valid_paths):
                    meta = self._catalog_meta(catalog, path)
                    for d in meta.get("dates", []):
                        y, m = d.get("year"), d.get("month")
                        if (target_years and y in target_years) or \
                           (target_months and m in target_months):
                            scores[i] *= 1.3
                            break

        # desc 정렬
        top_indices = sorted(range(len(scores)), key=lambda i: scores[i], reverse=True)[:self.top_k]

        results = []
        for idx in top_indices:
            if scores[idx] > 0.0:
                results.append({
                    "file_path": valid_paths[idx],
                    "score": round(float(scores[idx]), 4)
                })

        logger.info(f"BM25 검색 완료. {len(results)}개 선택됨")
        return results

    def _load_context(self, target_files: List[Dict], catalog: Dict = None) -> str:
        context_blocks = []
        current_len = 0

        for item in target_files:
            file_path = _normalize_doc_path(item["file_path"])
            fpath = self.target_dir / file_path

            if not fpath.exists():
                continue

            text = fpath.read_text(encoding='utf-8')

            # 시계열 인식을 위한 날짜 메타 주입
            date_str = ""
            if catalog:
                meta = self._catalog_meta(catalog, file_path)
                dates = meta.get("dates", [])
                valid = [f"{d['year']}-{d['month']:02d}" for d in dates
                         if d.get("year") and d.get("month")
                         and 1990 <= d["year"] <= 2030 and 1 <= d["month"] <= 12]
                if valid:
                    date_str = f" | Dates: {', '.join(valid)}"

            block = f"--- [Doc: {file_path}{date_str}] ---\n{text}\n\n"
            
            if current_len + len(block) > self.max_char_limit:
                allowed_len = self.max_char_limit - current_len
                if allowed_len > 100:
                    context_blocks.append(block[:allowed_len] + "\n...[Truncated]...")
                logger.warning(f"Context 제한 초과 ({self.max_char_limit} chars). Truncating.")
                break
            
            context_blocks.append(block)
            current_len += len(block)
                
        return "".join(context_blocks)

    def generate_stream(self, query: str, target_files: Union[List[str], List[Dict]],
                        search_query: str = None, catalog: Dict = None,
                        params: Dict = None,
                        chat_history: List[Dict] = None) -> Generator[Dict[str, Any], None, None]:
        if not target_files:
            yield {"answer": "조건에 부합하는 문서가 없어 답변할 수 없습니다.", "references": []}
            return

        bm25_targets = target_files
        if isinstance(target_files[0], str):
            bm25_query = search_query if search_query else query
            bm25_targets = self._retrieve_bm25(bm25_query, target_files,
                                                catalog=catalog, params=params)

        if not bm25_targets:
            yield {"answer": "본문 내에 일치하는 내용이 존재하지 않습니다.", "references": []}
            return

        context = self._load_context(bm25_targets, catalog=catalog)
        logger.info(f"Context 로드 완료. Length: {len(context)} chars")

        history_block = ""
        if chat_history:
            recent = chat_history[-10:]
            lines = []
            for msg in recent:
                role_label = "사용자" if msg["role"] == "user" else "시스템"
                lines.append(f"{role_label}: {msg['content']}")
            history_block = "\n[이전 대화]\n" + "\n".join(lines) + "\n"

        prompt = f"""다음 제공된 [Context] 문서들만 참고해서 [Query]에 대한 답변 작성.
Context에 없는 내용은 지어내지 말고, "해당 내용은 문서에서 확인할 수 없습니다"라고 할 것.
이전 대화가 있으면 맥락을 이어서 답변할 것. 사용자가 "다른건?", "더 없어?" 등 후속 질문을 하면 이전 대화 맥락을 참고할 것.
설명은 간결하고 핵심만.

[Context]
{context}
{history_block}
[Query]
{query}"""

        # vLLM (OpenAI 호환) 페이로드 구성
        payload = {
            "model": self.model_id,
            "messages": [{"role": "user", "content": prompt}],
            "stream": True,
            "temperature": 0.6
        }

        logger.info("스트리밍 추론 요청")
        start_t = time.time()
        first_token_received = False

        try:
            with requests.post(self.api_url, json=payload, stream=True, timeout=self.req_timeout) as res:
                res.raise_for_status()
                
                full_answer = ""
                for line in res.iter_lines():
                    if line:
                        decoded = line.decode('utf-8').strip()
                        
                        if decoded.startswith("data:"):
                            data_str = decoded[5:].strip()
                            
                            if data_str == "[DONE]":
                                break
                            if not data_str:
                                continue
                                
                            try:
                                chunk = json.loads(data_str)
                                content = chunk['choices'][0]['delta'].get('content', '')
                                
                                if content:
                                    if not first_token_received:
                                        ttft = time.time() - start_t
                                        logger.info(f"TTFT: {ttft:.2f}s")
                                        first_token_received = True

                                    full_answer += content
                                    yield {
                                        "answer": full_answer,
                                        "references": bm25_targets
                                    }
                            except json.JSONDecodeError:
                                logger.debug(f"JSON Parse err: {data_str}")
                                continue
                            except KeyError:
                                continue
                                
        except requests.exceptions.RequestException as e:
            logger.error(f"API Error: {e}")
            yield {
                "answer": f"답변 생성 중 에러 발생: {e}",
                "references": bm25_targets
            }
