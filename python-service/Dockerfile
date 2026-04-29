ARG PYTHON_BASE_IMAGE=python:3.11-slim
FROM ${PYTHON_BASE_IMAGE}
WORKDIR /app
ARG APP_UID=1000
ARG APP_GID=1000

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    chromium \
    fonts-noto-cjk \
    fonts-noto-color-emoji \
  && rm -rf /var/lib/apt/lists/*
RUN groupadd --gid "${APP_GID}" app \
  && useradd --uid "${APP_UID}" --gid app --create-home --shell /usr/sbin/nologin app

COPY pyproject.toml pyproject.toml
COPY app app
COPY legacy-md-indexer-main legacy-md-indexer-main
RUN pip install --no-cache-dir .
RUN mkdir -p /app/output \
  && chown -R app:app /app

ENV PARSER_SERVICE_HOST=0.0.0.0
ENV PARSER_SERVICE_PORT=8002
ENV PARSER_PDF_OUTPUT_DIR=/app/output/pdf
ENV PARSER_PDF_PUBLIC_BASE_URL=http://127.0.0.1:8002
ENV CHROMIUM_EXECUTABLE_PATH=/usr/bin/chromium
EXPOSE 8002
USER app
CMD ["sh", "-c", "uvicorn app.main:app --host ${PARSER_SERVICE_HOST} --port ${PARSER_SERVICE_PORT}"]
