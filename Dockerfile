# Dockerfile for aarch64 - using pre-built binary
# Build binary first: cargo build --release --target aarch64-unknown-linux-musl
# Build frontend first: cd frontend && npm install && npm run build

FROM ghcr.io/astral-sh/uv:0.12.21 AS uv

FROM alpine:3.19

RUN apk add --no-cache ca-certificates tzdata \
    && addgroup -S app && adduser -S -G app app

WORKDIR /app

# Copy pre-built binary
COPY target/aarch64-unknown-linux-musl/release/reader-rust /app/reader-rust

# Copy frontend dist
COPY frontend/dist /app/web/dist

# uv（来自官方镜像，不污染 apk）+ agent sidecar 及其依赖预热
COPY --from=uv /uv /usr/local/bin/uv
COPY sidecar /app/sidecar
# 版本以 sidecar/pyproject.toml 为唯一钉版源；uv sync 自动建 /app/sidecar/.venv
# 托管 Python 装到共享位置：默认在 /root/.local 下，运行时 app 用户穿不过去
ENV UV_PYTHON_INSTALL_DIR=/opt/uv-python
RUN uv sync --project /app/sidecar

# Create storage directory and hand ownership to the non-root user
# (SQLite database, file cache and uploaded assets all live under /app/storage)
RUN mkdir -p /app/storage/assets && chown -R app:app /app /opt/uv-python

# Drop privileges: the server must not run as root
USER app

# Environment defaults
ENV SERVER_HOST=0.0.0.0
ENV SERVER_PORT=8080
ENV DATABASE_URL=sqlite:/app/storage/reader.db?mode=rwc
ENV STORAGE_DIR=/app/storage
ENV ASSETS_DIR=/app/storage/assets
ENV WEB_ROOT=/app/web/dist
ENV LOG_LEVEL=info

EXPOSE 8080

VOLUME ["/app/storage"]

CMD ["./reader-rust"]
