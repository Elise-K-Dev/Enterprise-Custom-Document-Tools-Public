from fastapi import FastAPI
from pydantic import BaseModel

app = FastAPI(title="document-parser-service", version="public")


class SearchRequest(BaseModel):
    query: str


@app.get("/health")
def health() -> dict[str, str]:
    return {"status": "ok"}


@app.post("/search/query")
def search_documents_by_rank(request: SearchRequest) -> dict:
    return {
        "status": "demo",
        "query": request.query,
        "answer": "Public snapshot excludes private document corpora. Mount runtime data to enable search.",
        "references": [],
    }
