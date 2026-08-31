# syntax=docker/dockerfile:1

# ---------- 构建阶段 ----------
FROM rust:1.98-bookworm AS builder

# edition 2024 需 Rust >= 1.85。SQLite 由 sqlx 内嵌编译（libsqlite3-sys/bundled），
# TLS 走纯 Rust（ring），无需系统 libsqlite3；rust 官方镜像已自带 C 工具链。

WORKDIR /build
COPY . .
RUN cargo build --release --locked -p emrs-server

# ---------- 运行阶段 ----------
FROM debian:bookworm-slim AS runtime

# ffmpeg 预装：命中 probe::ensure_ffmpeg_binary 的已安装分支，避免运行时联网下载。
# 无系统 libsqlite3 依赖（SQLite 已内嵌）。
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
         ca-certificates ffmpeg tzdata \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /emrs
COPY --from=builder /build/target/release/emrs-server /usr/local/bin/emrs-server

# 配置(emrs.yml)与数据库(data/)均落在 /emrs，挂载此卷即可持久化
EXPOSE 8086
VOLUME ["/emrs"]

ENTRYPOINT ["/usr/local/bin/emrs-server"]
