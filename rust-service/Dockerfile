ARG RUST_BASE_IMAGE=rust:1.95
ARG DEBIAN_BASE_IMAGE=debian:bookworm-slim
FROM ${RUST_BASE_IMAGE} as builder
WORKDIR /app

COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock
COPY src src
RUN cargo build --release --locked --bin document-service

FROM ${DEBIAN_BASE_IMAGE}
WORKDIR /app
ARG APP_UID=1000
ARG APP_GID=1000
RUN apt-get update \
    && apt-get install -y --no-install-recommends libbz2-1.0 liblzma5 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN groupadd --gid "${APP_GID}" app \
    && useradd --uid "${APP_UID}" --gid app --create-home --shell /usr/sbin/nologin app
COPY --from=builder /app/target/release/document-service /usr/local/bin/document-service
COPY --chown=app:app templates /app/templates
RUN mkdir -p /app/DB \
    && chown -R app:app /app/DB
ENV DOCUMENT_SERVICE_HOST=0.0.0.0
ENV DOCUMENT_SERVICE_PORT=8001
EXPOSE 8001
USER app
CMD ["document-service"]
