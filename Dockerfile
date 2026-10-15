FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    chromium \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/x-poster /usr/local/bin/x-poster

ENV DEBUG_DIR=/data/debug \
    CHROME_BIN=/usr/bin/chromium \
    CHROME_NO_SANDBOX=true \
    HOST=0.0.0.0 \
    PORT=8000

VOLUME /data
EXPOSE 8000
CMD ["x-poster"]
