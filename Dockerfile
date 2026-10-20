# Dockerfile（amd64/arm64 通用，全链路多阶段：源码进、镜像出，可复现）。
#   docker build --build-arg TARGET_TRIPLE=x86_64-unknown-linux-musl -t reader-rust .
#   docker build --build-arg TARGET_TRIPLE=aarch64-unknown-linux-musl -t reader-rust .
# 不再依赖本机预编译二进制。

ARG TARGET_TRIPLE=x86_64-unknown-linux-musl

# ── 前端构建 ──
FROM node:22-alpine AS frontend
WORKDIR /build
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# ── 后端构建（musl 静态链接）──
FROM clux/muslrust:stable AS backend
ARG TARGET_TRIPLE
WORKDIR /build
# 依赖清单先拷：源码未变时 cargo 依赖层命中缓存
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN rustup target add ${TARGET_TRIPLE} \
    && cargo build --release --locked --target ${TARGET_TRIPLE}

FROM ghcr.io/astral-sh/uv:0.12.21 AS uv

FROM alpine:3.19
ARG TARGET_TRIPLE

RUN apk add --no-cache ca-certificates tzdata \
    && addgroup -S app && adduser -S -G app app

WORKDIR /app

# 后端二进制（构建阶段产物）
COPY --from=backend /build/target/${TARGET_TRIPLE}/release/reader-rust /app/reader-rust

# 前端产物（构建阶段产物）
COPY --from=frontend /build/dist /app/web/dist

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
