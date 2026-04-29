from fastapi import FastAPI
from pydantic import BaseModel

app = FastAPI(title="markdown-pdf-service", version="public")


class RenderRequest(BaseModel):
    title: str
    markdown: str


@app.get("/health")
def health() -> dict[str, str]:
    return {"status": "ok"}


@app.post("/render/markdown-pdf")
def render_markdown_pdf(request: RenderRequest) -> dict:
    return {
        "status": "demo",
        "title": request.title,
        "message": "Public snapshot excludes generated file storage. Mount runtime output storage to enable downloads.",
    }
