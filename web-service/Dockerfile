ARG PYTHON_BASE_IMAGE=python:3.11-slim
FROM ${PYTHON_BASE_IMAGE}
WORKDIR /app
ARG APP_UID=1000
ARG APP_GID=1000

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    fonts-noto-cjk \
    fonts-noto-color-emoji \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/*
RUN groupadd --gid "${APP_GID}" app \
  && useradd --uid "${APP_UID}" --gid app --create-home --shell /usr/sbin/nologin app

COPY pyproject.toml pyproject.toml
COPY app app
RUN pip install --no-cache-dir .
# Install Playwright Chromium + system dependencies (used by crawl4ai)
RUN playwright install --with-deps chromium
# Move browsers cache to a known path under the app user's home so it's available at runtime
ENV PLAYWRIGHT_BROWSERS_PATH=/home/app/.cache/ms-playwright
RUN mkdir -p "${PLAYWRIGHT_BROWSERS_PATH}" \
  && cp -r /root/.cache/ms-playwright/* "${PLAYWRIGHT_BROWSERS_PATH}/" \
  && chown -R app:app /app /home/app/.cache

ENV WEB_SERVICE_HOST=0.0.0.0
ENV WEB_SERVICE_PORT=8004
ENV WEB_SERVICE_PUBLIC_BASE_URL=http://127.0.0.1:8004
EXPOSE 8004

USER app
CMD ["sh", "-c", "uvicorn app.main:app --host ${WEB_SERVICE_HOST} --port ${WEB_SERVICE_PORT}"]
