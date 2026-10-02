FROM rust:1.99.0-alpine3.24 AS builder

WORKDIR /app

RUN apk add --no-cache build-base ca-certificates git pkgconf

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web

# Cache the registry and build output across builds; the binary is copied out because cache mounts are not layers.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --locked --release \
    && cp target/release/tree-sitter-playground /usr/local/bin/tree-sitter-playground

FROM alpine:3.24

LABEL org.opencontainers.image.source="https://github.com/joowani/tree-sitter-playground" \
      org.opencontainers.image.description="Web playground for exploring Tree-sitter syntax trees" \
      org.opencontainers.image.licenses="MIT"

RUN apk add --no-cache ca-certificates && adduser -D -u 10001 playground

COPY --from=builder /usr/local/bin/tree-sitter-playground /usr/local/bin/tree-sitter-playground

EXPOSE 3000

USER playground

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s \
    CMD wget -q -O /dev/null http://127.0.0.1:3000/api/languages || exit 1

CMD ["tree-sitter-playground", "--host", "0.0.0.0", "--port", "3000"]
