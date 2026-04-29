"""
Project: Data RAG System
Module: MD Catalog Builder (Track 1 & Sanitizer & Track 2 Ollama)
Description: 
    - processed_md 파일 대상 정제 및 메타데이터 1차 추출
    - Crash-safe JSON Dump (단일 파일 처리 단위)
    - Track 1 실패 시 PENDING_LLM 상태로 마킹하여 Track 2(Ollama) 큐로 라우팅
"""

import os
import json
import re
import time
import logging
import requests
import configparser
from pathlib import Path
from typing import Dict, List, Tuple, Optional
from tqdm import tqdm

# 로거 셋업
logger = logging.getLogger("CATALOG_BUILDER")
logger.setLevel(logging.INFO)
fmt = logging.Formatter('[%(levelname)s] %(message)s')
ch = logging.StreamHandler()
ch.setFormatter(fmt)
logger.addHandler(ch)

# Track 2 전용 로거
logger_t2 = logging.getLogger("TRACK2_OLLAMA")
logger_t2.setLevel(logging.INFO)
ch2 = logging.StreamHandler()
ch2.setFormatter(logging.Formatter('[%(levelname)s] %(asctime)s - %(message)s', '%H:%M:%S'))
logger_t2.addHandler(ch2)

def sanitize_md_for_rag(text: str) -> str:
    """RAG 청킹 최적화를 위한 노이즈 정제"""
    text = re.sub(r'', '', text)
    text = re.sub(r'[^\w\s\n\.,!\?\-\|#\(\)\[\]<>\'":;/%&~+*=]', '', text)
    text = re.sub(r'[ \t]+', ' ', text)
    text = re.sub(r'\n{3,}', '\n\n', text)
    return text.strip()

class RegexFastTrack:
    """Track 1: 정규식 기반 고속 분류기"""
    def __init__(self):
        self.doc_type_patterns = [
            r'(주간업무|업무보고|일일보고|일일\s*주요\s*정비사항)',
            r'(회의록|의사록)',
            r'(견적서|발주서|품의서)',
            r'(검수|정기검사)',
            r'(계획서|기획안)',
            r'(운영\s*현황|BEP계산|월간업무보고)'
        ]

    def process(self, file_path: Path, content_head: str) -> Dict:
        dates = self._extract_dates(file_path, content_head)
        doc_type = self._extract_doc_type(file_path)

        return {
            "doc_type": doc_type,
            "dates": dates,
            "is_complete": bool(dates and doc_type)
        }

    def _extract_dates(self, file_path: Path, content_head: str) -> List[Dict[str, int]]:
        """문서에서 발견되는 모든 (year, month) 쌍을 중복 없이 반환"""
        found: List[Tuple[int, int]] = []

        def _to_year(val: int) -> int:
            return val if val >= 2000 else 2000 + val

        def _add(y: int, m: int):
            entry = (_to_year(y), m)
            if entry not in found:
                found.append(entry)

        filename = file_path.name
        date_patterns = [
            r'(?<!\d)((?:20)?\d{2})[-_.\s](1[0-2]|0?[1-9])[-_.\s](3[01]|[12]\d|0?[1-9])(?!\d)',
            r'(?<!\d)((?:20)?\d{2})(1[0-2]|0[1-9])(3[01]|[12]\d|0[1-9])(?!\d)'
        ]
        for pattern in date_patterns:
            for m in re.finditer(pattern, filename):
                _add(int(m.group(1)), int(m.group(2)))

        # 경로/파일명의 XX년 YY월 패턴
        path_str = str(file_path).replace(' ', '')
        y_matches = list(re.finditer(r'((?:20)?\d{2})년', path_str))
        m_matches = list(re.finditer(r'(1[0-2]|0?[1-9])월', path_str))
        if y_matches and m_matches:
            _add(int(y_matches[0].group(1)), int(m_matches[0].group(1)))

        # 문서 본문 전체에서 날짜 패턴 탐색
        if content_head:
            body_patterns = [
                r'(?:작성일|일자|보고일|기준일|보고월)\s*[:\s]*((?:20)?\d{2})[\.\-년\s/]+(1[0-2]|0?[1-9])[\.\-월\s/]+',
                r'(?<!\d)((?:20)?\d{2})[\.\-/]\s*(1[0-2]|0?[1-9])[\.\-/]\s*(3[01]|[12]\d|0?[1-9])(?!\d)',
                r'((?:20)?\d{2})년\s*(1[0-2]|0?[1-9])월',
            ]
            for pattern in body_patterns:
                for m in re.finditer(pattern, content_head):
                    _add(int(m.group(1)), int(m.group(2)))

        return [{"year": y, "month": mo} for y, mo in found]

    def _extract_doc_type(self, file_path: Path) -> Optional[str]:
        full_path_str = str(file_path).replace(' ', '')
        for pattern in self.doc_type_patterns:
            match = re.search(pattern, full_path_str)
            if match: return match.group(1)
        return None

class CatalogBuilder:
    """Track 1 메인 로직"""
    def __init__(self, target_dir: str, output_json: str):
        self.target_dir = Path(target_dir)
        self.output_json = Path(output_json)
        self.fast_track = RegexFastTrack()
        self.catalog = self._load_existing_catalog()

    def _load_existing_catalog(self) -> dict:
        if self.output_json.exists():
            try:
                with open(self.output_json, 'r', encoding='utf-8') as f:
                    return json.load(f)
            except json.JSONDecodeError:
                logger.error(f"JSON 손상. 백업본 생성 필요: {self.output_json.name}")
        return {}

    def _dump_catalog(self):
        with open(self.output_json, 'w', encoding='utf-8') as f:
            json.dump(self.catalog, f, ensure_ascii=False, indent=2)

    def run_pipeline(self):
        if not self.target_dir.exists():
            logger.error(f"타겟 디렉토리 없음: {self.target_dir}")
            return

        md_files = list(self.target_dir.rglob('*.md'))
        logger.info(f"파싱 대상: Total {len(md_files)}ea")

        start_t = time.time()
        track1_hit = 0
        track2_route = 0
        skip_cnt = 0

        for fpath in md_files:
            file_key = fpath.relative_to(self.target_dir).as_posix()
            
            if file_key in self.catalog:
                status = self.catalog[file_key].get("status")
                if status in ["COMPLETED", "PENDING_LLM"]:
                    skip_cnt += 1
                    continue

            try:
                raw_text = fpath.read_text(encoding='utf-8')
                clean_text = sanitize_md_for_rag(raw_text)
            except Exception as e:
                logger.error(f"파일 Read 에러 [{fpath.name}]: {e}")
                continue

            content_head = clean_text[:500]
            meta = self.fast_track.process(fpath, content_head)
            
            if meta.pop("is_complete"):
                self.catalog[file_key] = {
                    "status": "COMPLETED",
                    "doc_type": meta["doc_type"],
                    "dates": meta["dates"],
                    "source": "Track1"
                }
                track1_hit += 1
            else:
                self.catalog[file_key] = {
                    "status": "PENDING_LLM",
                    "partial_meta": meta,
                    "source": "Track2_Queue"
                }
                track2_route += 1

            self._dump_catalog()

        elapsed = time.time() - start_t
        logger.info(f"Track 1 파이프라인 종료. (Elapsed: {elapsed:.2f}s)")
        logger.info(f"Result - Track1 Hit: {track1_hit}, Track2 Queue: {track2_route}, Skips: {skip_cnt}")


class OllamaFallbackRouter:
    """Track 2: PENDING_LLM 대상 로컬 Ollama Zero-shot 분류기"""
    def __init__(self, catalog_path: str, target_dir: str):
        self.catalog_path = Path(catalog_path)
        self.target_dir = Path(target_dir)
        self.catalog = self._load_catalog()
        
        config = configparser.ConfigParser()
        config.read('setting.conf')
        self.model_id = config['DEFAULT'].get('MODEL_ID', 'nemotron-cascade-2')
        self.api_url = config['DEFAULT'].get('API_URL', 'http://hai-server:8000/v1/completions')

    def _load_catalog(self) -> dict:
        if self.catalog_path.exists():
            with open(self.catalog_path, 'r', encoding='utf-8') as f:
                return json.load(f)
        raise FileNotFoundError("catalog 파일 누락. Track 1 선행 필요")

    def _dump_catalog(self):
        with open(self.catalog_path, 'w', encoding='utf-8') as f:
            json.dump(self.catalog, f, ensure_ascii=False, indent=2)

    def _extract_json(self, text: str) -> dict:
        clean_text = re.sub(r'<think>.*?</think>', '', text, flags=re.DOTALL)
        clean_text = re.sub(r'```json|```', '', clean_text).strip()
        
        match = re.search(r'\{.*\}', clean_text, re.DOTALL)
        if match:
            json_str = match.group(0)
            try:
                # strict=False allows unescaped control chars like \n inside strings
                return json.loads(json_str, strict=False)
            except json.JSONDecodeError as e:
                logger_t2.error(f"JSON decode 에러: {e} | Raw: {json_str[:150]}")
        else:
            logger_t2.error(f"JSON 패턴 매칭 실패 | Raw: {clean_text[:150]}")
            
        # Fallback: Regex extraction if JSON parsing completely fails
        try:
            result = {}
            dt_match = re.search(r'"doc_type"\s*:\s*"([^"]+)"', clean_text)
            result["doc_type"] = dt_match.group(1) if dt_match else None

            dates = []
            for ym in re.finditer(r'"year"\s*:\s*(\d{4})[^}]*?"month"\s*:\s*(\d{1,2})', clean_text):
                dates.append({"year": int(ym.group(1)), "month": int(ym.group(2))})
            result["dates"] = dates

            if result["doc_type"] or dates:
                return result
        except Exception:
            pass

        return {}

    def _call_ollama(self, prompt: str, system_prompt: str = "") -> str:
        payload = {
            "model": self.model_id,
            "prompt": prompt,
            "system": system_prompt,
            "stream": False,
            "format": {
                "type": "object",
                "properties": {
                    "doc_type": {"type": ["string", "null"]},
                    "dates": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "year": {"type": "integer"},
                                "month": {"type": "integer"}
                            }
                        }
                    }
                },
                "required": ["doc_type", "dates"]
            },
            "options": {
                "temperature": 0.1,
                "num_ctx": 4096
            }
        }
        for attempt in range(3):
            try:
                res = requests.post(self.api_url, json=payload, timeout=300)
                res.raise_for_status()
                return res.json().get("response", "")
            except requests.exceptions.RequestException as e:
                logger_t2.warning(f"Ollama API Error (Attempt {attempt+1}/3): {e}")
                if attempt == 2:
                    logger_t2.error(f"Ollama API completely failed: {e}")
                    return ""
                time.sleep(2)
        return ""

    def run(self):
        pending_keys = [k for k, v in self.catalog.items() if v.get("status") == "PENDING_LLM"]
        if not pending_keys:
            logger_t2.info("PENDING_LLM task 없음. 종료.")
            return

        logger_t2.info(f"Ollama Fallback 시작: Total {len(pending_keys)}ea (Model: {self.model_id})")

        # tqdm 적용: 진행률과 예상 시간(ETA) 표기
        for key in tqdm(pending_keys, desc="Ollama Inference", unit="file"):
            start_t = time.time()
            fpath = self.target_dir / key
            
            if not fpath.exists():
                logger_t2.warning(f"Target 파일 누락: {key}")
                continue

            try:
                raw_text = fpath.read_text(encoding='utf-8')
                trunc_text = raw_text[:2000] # Context Window 초과 방지를 위해 길이 축소
            except Exception as e:
                logger_t2.error(f"파일 읽기 실패 [{key}]: {e}")
                continue

            system_prompt = "You are a data extraction bot. You must extract doc_type and all dates mentioned. You must output ONLY a valid JSON object. Do not output any conversational text or explanation."
            prompt = f"""다음 문서에서 doc_type과 문서에 등장하는 모든 날짜(year/month)를 추출하라. 알 수 없는 항목은 null 또는 빈 배열로 처리할 것.
반드시 JSON 포맷으로만 응답하고, 마크다운 기호나 추가 설명은 절대 포함하지 마라.
응답 예시: {{"doc_type": "보고서", "dates": [{{"year": 2024, "month": 3}}, {{"year": 2024, "month": 4}}]}}

[문서내용]
{trunc_text}"""

            res_text = self._call_ollama(prompt, system_prompt=system_prompt)
            if not res_text:
                self.catalog[key]["status"] = "ERROR"
                self._dump_catalog()
                continue

            parsed_data = self._extract_json(res_text)
            
            if parsed_data:
                partial_meta = self.catalog[key].get("partial_meta", {})
                # Track 1에서 찾은 날짜 + Ollama가 추가로 찾은 날짜를 병합 (중복 제거)
                existing_dates = partial_meta.get("dates", [])
                ollama_dates = parsed_data.get("dates") or []
                merged = existing_dates[:]
                for d in ollama_dates:
                    if d not in merged:
                        merged.append(d)
                self.catalog[key].update({
                    "status": "COMPLETED",
                    "doc_type": parsed_data.get("doc_type"),
                    "dates": merged,
                    "source": "Track2_Ollama"
                })
                # 진행률 바와 겹치지 않게 로그 출력을 제어하려면 logger.info 대신 tqdm.write 사용 권장
                # 여기서는 콘솔 출력의 깔끔함을 위해 성공 로그는 생략하거나 주석 처리함.
                # tqdm.write(f"[INFO] Ollama parsing 성공: {key} ({time.time() - start_t:.2f}s)")
            else:
                self.catalog[key]["status"] = "FAILED"
                logger_t2.error(f"데이터 추출 실패: {key}")

            self._dump_catalog()

if __name__ == "__main__":
    target = "./processed_md"
    catalog_out = "./file_catalog.json"
    
    # 1. Regex Fast-track 실행
    builder = CatalogBuilder(target_dir=target, output_json=catalog_out)
    builder.run_pipeline()
    
    # 2. Track 1 종료 후, 실패 건들에 대해 Ollama Fallback 실행
    ollama_router = OllamaFallbackRouter(catalog_path=catalog_out, target_dir=target)
    ollama_router.run()
