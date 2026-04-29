from fastapi import FastAPI
from pydantic import BaseModel, HttpUrl

app = FastAPI(title="web-fetch-service", version="public")


class FetchRequest(BaseModel):
    url: HttpUrl


@app.get("/health")
def health() -> dict[str, str]:
    return {"status": "ok"}


@app.post("/web/fetch")
def fetch_web_page(request: FetchRequest) -> dict:
    return {
        "status": "demo",
        "url": str(request.url),
        "markdown": "Public snapshot stub. Add your own fetch implementation or use the private service implementation.",
    }
