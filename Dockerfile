FROM rust:1.85-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock* ./
COPY migrations ./migrations
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home app

COPY --from=builder /app/target/release/telegram-groq-bot /usr/local/bin/telegram-groq-bot
USER app
EXPOSE 8080
ENTRYPOINT ["telegram-groq-bot"]
CMD ["serve"]

