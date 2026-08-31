# EMRS

Emby 兼容媒体服务器（Rust 实现）。目标：让 Emby 客户端（Infuse 等）直接对接——登录、媒体库、条目、播放、收藏/进度、扫描入库、云盘 302 全走 Emby 线协议。

## Docker 用法

镜像内含 `emrs-server` 二进制与 ffmpeg，监听端口 **8086**。配置 `emrs.yml` 与数据库 `data/emrs.db` 默认都落在工作目录 `/emrs`，**挂载 `/emrs` 一个卷即可持久化全部状态**。

### 获取镜像

**方式一：从 Release 下载镜像包加载**（无需编译，推荐）

发布 Release 会附带 `emrs-<tag>-docker.tar.gz`，加载后即可运行：

```bash
docker load < emrs-v0.1.0-docker.tar.gz
docker images | grep emrs
```

**方式二：本地构建**

```bash
docker build -t emrs:local .
```

### 运行

```bash
# 用命名卷持久化
docker run -d \
  --name emrs \
  -p 8086:8086 \
  -v emrs-data:/emrs \
  --restart unless-stopped \
  emrs:local
```

挂载本地目录也可以（便于直接改配置）：

```bash
docker run -d \
  --name emrs \
  -p 8086:8086 \
  -v "$PWD/emrs-docker:/emrs" \
  --restart unless-stopped \
  emrs:local
```

挂载媒体库（扫描入库需要容器内可访问媒体路径）：

```bash
docker run -d \
  --name emrs \
  -p 8086:8086 \
  -v emrs-data:/emrs \
  -v /path/to/media:/media:ro \
  --restart unless-stopped \
  emrs:local
```

之后在后台管理接口创建媒体库时，路径填容器内的 `/media/...`。

### docker-compose

```yaml
services:
  emrs:
    image: emrs:local        # 或改成加载后的 emrs:v0.1.0
    container_name: emrs
    ports:
      - "8086:8086"
    volumes:
      - emrs-data:/emrs
      - /path/to/media:/media:ro
    restart: unless-stopped

volumes:
  emrs-data:
```

```bash
docker compose up -d
```

### 首次启动注意事项

- **配置自动生成后会退出一次**：若 `/emrs` 下没有 `emrs.yml`，程序会写入默认配置并退出，日志提示「请修改配置后重新启动」。配合 `--restart unless-stopped`，容器会自动重启并以默认配置继续运行（默认管理员密码打印在日志中）。想避免这次重启，可先手动放一份 `emrs.yml` 到挂载目录。
- **密钥来自配置文件，不读环境变量**：`playback.signing_key`、`tmdb.api_key` 等必须写进 `emrs.yml`（见 `crates/emrs-core/resource/default.yml` 的字段说明）。生产环境务必改掉默认 `signing_key` 与管理员密码。
- **代理**：如需 TMDB 抓取/剧照下载走代理，在 `emrs.yml` 的 `http.proxy_url` 配置；容器内 `127.0.0.1` 指向容器自身，宿主机代理请用宿主机内网 IP 或 `host.docker.internal`。

### 常用运维命令

```bash
docker logs -f emrs          # 查看日志（含初始管理员密码）
docker exec -it emrs sh      # 进入容器
docker stop emrs && docker rm emrs   # 停止并移除（数据在卷中，不丢）
```

## 二进制运行（Release 产物）

每个 Release 附带三个产物：

| 产物 | 说明 |
|---|---|
| `emrs-<tag>-x86_64-unknown-linux-musl` | Linux x86_64 静态二进制（无 glibc 依赖，裸文件不压缩） |
| `emrs-<tag>-x86_64-pc-windows-msvc.exe` | Windows x86_64 可执行文件 |
| `emrs-<tag>-docker.tar.gz` | Docker 镜像包（`docker load` 用） |

SQLite 已内嵌编译、TLS 走纯 Rust（ring），二进制开箱即用。ffmpeg 由 `ffmpeg-sidecar` 首次运行时自动下载，也可自行安装 `ffmpeg`/`ffprobe` 到 `PATH`。

```bash
# Linux（下载即二进制，加执行权限运行）
chmod +x emrs-v0.1.0-x86_64-unknown-linux-musl
./emrs-v0.1.0-x86_64-unknown-linux-musl   # 在当前目录生成 emrs.yml 与 data/

# Windows（PowerShell，直接运行）
.\emrs-v0.1.0-x86_64-pc-windows-msvc.exe
```

首次运行若当前目录无 `emrs.yml`，程序会写入默认配置后退出并提示「请修改配置后重新启动」——按提示改完再启动即可。
