"""
Phase 2 Agentic Router
- 메타데이터(연도, 월) 기반 Pre-filtering
- 텍스트 키워드 검색은 Phase 3(BM25)로 위임
"""
import json
import logging
import requests
import configparser
import re
from pathlib import Path
from typing import List, Dict, Any

logger = logging.getLogger("ROUTER")
logger.setLevel(logging.INFO)
ch = logging.StreamHandler()
ch.setFormatter(logging.Formatter('[%(levelname)s] %(message)s'))
logger.addHandler(ch)

class AgenticRouter:
    def __init__(self, catalog_path: str):
        self.catalog_path = Path(catalog_path)
        self.catalog = self._load_catalog()
        
        config = configparser.ConfigParser()
        config.read('setting.conf')
        self.model_id = config['DEFAULT'].get('MODEL_ID', 'gemma-4-31b-it')
        # vLLM 호환 엔드포인트 기본값 변경
        self.api_url = config['DEFAULT'].get('API_URL', 'http://hai-server:8000/v1/chat/completions')

    def _load_catalog(self) -> dict:
        if not self.catalog_path.exists():
            raise FileNotFoundError("catalog.json missing")
        with open(self.catalog_path, 'r', encoding='utf-8') as f:
            return json.load(f)

    def _clean_query(self, query: Any) -> str:
        # 입력 형태 보정
        if isinstance(query, list) and len(query) > 0 and isinstance(query[0], dict):
            return query[0].get('text', str(query))
        if isinstance(query, dict):
            return query.get('text', str(query))
        return str(query)

    def _extract_parameters(self, query: str) -> dict:
        current_year = 2026
        
        # 키워드 분리 및 검색용 질의 추출
        prompt = f"""
        질문 분석 후 JSON 응답.

        [조건]
        - years: 연도 (list of int). 현재 {current_year}년 기준. (예: 최근 2년 -> [2025, 2026])
        - months: 월 (list of int). 범위 질의는 모든 월을 나열 (예: 3월~6월 -> [3, 4, 5, 6]). 조건 없으면 []
        - search_query: 본문 검색용 질의어 (string). 연도/월을 제외한 핵심 키워드만 추출

        [질문]
        "{query}"

        [출력]
        {{"years": [2025, 2026], "months": [], "search_query": "엘리베이터 수리 내역"}}
        """

        # vLLM (OpenAI) 호환 페이로드로 변경
        payload = {
            "model": self.model_id,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0.0
        }

        try:
            res = requests.post(self.api_url, json=payload, timeout=60)
            res.raise_for_status()
            
            # vLLM 양식에 맞게 응답 텍스트 파싱
            res_text = res.json()['choices'][0]['message']['content']

            clean_text = re.sub(r'<think>.*?</think>', '', res_text, flags=re.DOTALL).strip()
            clean_text = re.sub(r'```json|```', '', clean_text).strip()

            try:
                params = json.loads(clean_text)
            except json.JSONDecodeError:
                match = re.search(r'\{[\s\S]*\}', clean_text)
                if match:
                    params = json.loads(match.group(0))
                else:
                    logger.error(f"JSON 파싱 실패. raw: {res_text}")
                    return {"years": [], "months": [], "search_query": query}

            # 추출된 파라미터 유효성 검증
            params["years"] = [y for y in (params.get("years") or []) if isinstance(y, int) and 1990 <= y <= 2030]
            params["months"] = [m for m in (params.get("months") or []) if isinstance(m, int) and 1 <= m <= 12]
            params.setdefault("search_query", query)
            return params

        except Exception as e:
            logger.error(f"파라미터 추출 에러: {e}")
            return {"years": [], "months": [], "search_query": query}

    def _filter_catalog(self, params: dict, user_rank: str = "hi_rank") -> List[str]:
        target_years = params.get("years") or []
        target_months = params.get("months") or []

        filtered_files = []

        for file_path, meta in self.catalog.items():
            if meta.get("status") != "COMPLETED":
                continue

            # 권한 필터링
            if user_rank == "low_rank" and "low_rank/" not in file_path:
                continue

            # 시간 조건 없을 경우 전체 통과
            if not target_years and not target_months:
                filtered_files.append(file_path)
                continue

            dates = meta.get("dates", [])
            matched = False
            for d in dates:
                y, m = d.get("year"), d.get("month")
                
                # 유효하지 않은 날짜 스킵
                if y is not None and (y < 1990 or y > 2030):
                    continue
                if m is not None and (m < 1 or m > 12):
                    continue
                    
                year_ok = (not target_years) or (y in target_years)
                month_ok = (not target_months) or (m in target_months)
                
                if year_ok and month_ok:
                    matched = True
                    break

            if matched:
                filtered_files.append(file_path)

        return filtered_files

    def route_query(self, raw_query: Any, user_rank: str = "hi_rank") -> Dict[str, Any]:
        clean_q = self._clean_query(raw_query)
        logger.info(f"질의: {clean_q} (권한: {user_rank})")
        
        params = self._extract_parameters(clean_q)
        logger.info(f"추출된 파라미터: {params}")

        target_files = self._filter_catalog(params, user_rank)
        
        # OOM 방지 (최대 500개)
        MAX_DOCS = 500 
        if len(target_files) > MAX_DOCS:
            logger.warning(f"문서 초과 ({len(target_files)}개). {MAX_DOCS}개로 자름")
            target_files = target_files[:MAX_DOCS]
            
        logger.info(f"필터링된 문서: {len(target_files)}개")

        return {
            "query": clean_q,
            "parameters": params,
            "target_files": target_files,
            "search_query": params.get("search_query", clean_q)
        }