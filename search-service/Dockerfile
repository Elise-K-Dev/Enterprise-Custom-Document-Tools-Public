ARG PYTHON_BASE_IMAGE=python:3.11-slim
FROM ${PYTHON_BASE_IMAGE}
WORKDIR /app
ARG APP_UID=1000
ARG APP_GID=1000

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    fonts-noto-cjk \
  && rm -rf /var/lib/apt/lists/*
RUN groupadd --gid "${APP_GID}" app \
  && useradd --uid "${APP_UID}" --gid app --create-home --shell /usr/sbin/nologin app

COPY pyproject.toml pyproject.toml
COPY app app
COPY legacy-md-indexer-main legacy-md-indexer-main
RUN pip install --no-cache-dir .
RUN mkdir -p /app/output \
  && chown -R app:app /app

ENV SEARCH_SERVICE_HOST=0.0.0.0
ENV SEARCH_SERVICE_PORT=8002
EXPOSE 8002
USER app
CMD ["sh", "-c", "uvicorn app.main:app --host ${SEARCH_SERVICE_HOST} --port ${SEARCH_SERVICE_PORT}"]
