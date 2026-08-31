# syntax=docker/dockerfile:1

# ---------- 构建阶段 ----------
FROM rust:1.98-bookworm AS builder

# edition 2024 需 Rust >= 1.85；libsqlite3 由 sqlx 链接系统库
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .
RUN cargo build --release --locked -p emrs-server

# ---------- 运行阶段 ----------
FROM debian:bookworm-slim AS runtime

# ffmpeg 预装：命中 probe::ensure_ffmpeg_binary 的已安装分支，避免运行时联网下载
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
         ca-certificates libsqlite3-0 ffmpeg tzdata \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /emrs
COPY --from=builder /build/target/release/emrs-server /usr/local/bin/emrs-server

# 配置(emrs.yml)与数据库(data/)均落在 /emrs，挂载此卷即可持久化
EXPOSE 8086
VOLUME ["/emrs"]

ENTRYPOINT ["/usr/local/bin/emrs-server"]
