FROM rust:1.75-slim AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/enterprise-document-service-public /usr/local/bin/document-service
EXPOSE 8001
CMD ["document-service"]
