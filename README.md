# Bangumi Recorder

自托管的媒体追踪记录器，基于 [Bangumi (bgm.tv)](https://bgm.tv) 和 IMDb 数据，支持搜索、添加条目、记录观看/阅读/游玩进度。

## 功能

- **Bangumi 搜索** — 按标题或 Bangumi ID 搜索动画 / 书籍 / 游戏 / 三次元等条目
- **IMDb 搜索** — 支持免 API 的 IMDb suggestion 检索，也可配置 OMDb API Key 使用 API 检索
- **追踪管理** — 添加 Bangumi、IMDb 或自定义条目到个人追踪列表，支持多种状态、软删除和硬删除
- **进度记录** — 以 `编号|分钟:秒` 格式记录进度（如 `5|2:12`），时间可为空（如 `5`）
- **JWT 认证** — 用户注册 / 登录，Bearer Token 鉴权
- **API Token 管理** — 支持多 Token、精细权限控制（只读/读写/查看信息/修改信息/添加记录/删除记录/修改记录/更改状态/全部允许），每个 Token 可独立设置权限组合
- **记录日志** — 记录创建、删除、进度、状态、单集进度等操作，可用于审计和年度总结
- **用户级自动清理** — 每个用户可独立开启软删除记录 30 天后自动物理删除；服务启动时执行一次，之后每天服务器本地时间 0 点执行
- **v2 API** — 统一的 `{status, data, message}` 响应格式 + HTTP 状态码，RESTful 路径设计
- **自托管** — 数据完全由自己掌控，MariaDB 存储

## 技术栈

| 层级 | 技术 |
|------|------|
| 后端 | Rust (Edition 2024), Axum 0.7, Tokio, SQLx (MariaDB/MySQL 协议) |
| 前端 | React 19, Next.js 16 App Router, TypeScript, Tailwind CSS 4, shadcn/ui + Radix UI, Motion, TanStack Query |
| 数据库 | MariaDB 11.7+ |
| 数据来源 | 抓取 [bgm.tv](https://bgm.tv)、IMDb suggestion / OMDb API |

## 前置条件

- [Rust](https://rustup.rs/) (Edition 2024)
- [Node.js](https://nodejs.org/) >= 20.9
- MariaDB 11.7+ 数据库（使用原生 `UUID_v7()`）
- [sqlx-cli](https://crates.io/crates/sqlx-cli)（用于数据库迁移）

## 快速开始

### 1. 配置环境变量

```shell
cp .env.example .env
```

编辑 `.env`：

```env
LISTEN=127.0.0.1
LISTEN_PORT=8080
RUST_LOG=info
DATABASE_URL=mysql://用户名:密码@localhost/数据库名
JWT_SECRET=你的随机密钥

# 可选：IMDb API 检索。未配置时使用免 API 的 IMDb suggestion/页面抓取。
# OMDB_API_KEY=你的 OMDb API Key
# IMDB_SEARCH_MODE=no_api

# 可选：指定外部前端地址（开发时使用，会跳过嵌入的静态文件）
# --frontend-url http://localhost:5173
```

### 2. 数据库迁移

```shell
sqlx migrate run
```

### 3. 构建并运行后端

前端会在 `cargo build`/`cargo run` 时自动构建（需要 Node.js）。也可手动构建：

```shell
cd frontend && npm ci --include=dev && npm run build
```

```shell
# 开发模式（使用内置前端）
cargo run

# 前端开发服务器（另开终端；API 指向 Rust 后端）
cd frontend
NEXT_PUBLIC_API_BASE_URL=http://127.0.0.1:8080 npm run dev

# Rust 后端将浏览器入口重定向到 Next.js 开发服务器
cargo run -- -f http://localhost:3001

# 开发模式（使用本地构建的前端目录）
cargo run -- -f ./frontend/dist

# 生产模式
cargo build --release
```

服务启动后访问 `http://127.0.0.1:8080`。

### Docker（手动运行）

镜像在构建时编译前端静态文件并嵌入 Rust 二进制；运行镜像只包含该二进制及其 TLS 证书。

```shell
docker build -t bangumi-recorder:local .
docker run --rm -p 8080:8080 --env-file .env \
  -e LISTEN=0.0.0.0 \
  bangumi-recorder:local
```

容器需能访问 `DATABASE_URL` 指向的 MariaDB。请在首次启动前执行数据库迁移；Dockerfile 不会自动执行迁移。`JWT_SECRET` 等敏感变量只应在运行时传入，不应写入镜像。

### Docker 快速部署（Compose）

这是推荐的自托管方式。Compose 会依次启动 MariaDB、一次性 `migrate` 服务和应用；`migrate` 使用 Dockerfile 中独立构建的 `sqlx-cli` 执行 `migrations/*.up.sql`，并通过 SQLx 的 `_sqlx_migrations` 记录表追踪版本。应用仅会在迁移成功后启动；历史迁移不会重复执行，且 SQLx 会校验它们未被修改。MariaDB 11.7+ 原生提供 `UUID_v7()`，不需要额外创建兼容函数。

所有服务使用宿主机网络，适用于 Linux Docker Engine；Docker Desktop 4.34+ 需先启用 host networking。应用直接监听宿主机的 `APP_PORT`（默认 `8080`），MariaDB 仅监听 `127.0.0.1:MYSQL_PORT`（默认 `3306`）。这两个端口需要可用；若宿主机已有数据库，请在 `.env` 中设置其他 `MYSQL_PORT`。

如需通过宿主机代理访问 Bangumi，可在 `.env` 中设置 `HTTP_PROXY=http://127.0.0.1:10808` 和 `HTTPS_PROXY=http://127.0.0.1:10808`（按实际代理端口调整）。这些变量会传给运行中的应用，构建时的 `--build-arg` 不会替代此配置。使用 `pkexec` 时也建议写入 `.env`，因为终端环境变量可能不会保留。

1. 安装 Docker Engine 和 Docker Compose plugin，并确认命令可用：

   ```shell
   docker compose version
   ```

2. 创建部署配置：

   ```shell
   cp compose.env.example .env
   openssl rand -hex 32
   ```

   编辑 `.env`，将 `MYSQL_PASSWORD`、`MYSQL_ROOT_PASSWORD` 和 `JWT_SECRET` 替换为随机值。`MYSQL_PASSWORD` 会用于构造数据库 URL，建议只使用字母、数字、`-` 与 `_`。

3. 构建并在后台启动全部服务：

   ```shell
   docker compose up --build -d
   ```

   首次启动时，`migrate` 会创建数据库表并正常退出；这是预期行为，不表示服务异常。

4. 确认服务已就绪：

   ```shell
   docker compose ps
   docker compose logs migrate
   docker compose logs -f app
   ```

   `migrate` 应以状态码 `0` 退出，随后当 `app` 显示为 healthy 后，访问 `http://127.0.0.1:8080`。如需修改对外端口，在 `.env` 中设置 `APP_PORT`，例如 `APP_PORT=18080`。

常用维护命令：

```shell
# 停止服务，保留数据库数据
docker compose down

# 更新镜像/代码后重新构建并启动；如有新迁移，migrate 会只执行新增项
docker compose up --build -d

# 停止服务并删除数据库数据；下次启动会从头执行全部迁移
docker compose down -v
```

数据库保存在名为 `mariadb-data` 的 Docker 卷。不要在已有生产数据的环境中执行 `docker compose down -v`。代理地址和密钥通过环境变量或未纳入 Git 的 `.env` 配置，不应直接写入 Compose 文件。

> 从此前的 MySQL Compose 部署切换到本配置时，不能复用原来的 `mysql-data` 卷。若其中只是测试数据，先执行 `docker compose down -v --remove-orphans`，再按上述步骤启动；生产数据则应先备份并制订 MySQL 到 MariaDB 的迁移方案，不能直接挂载或清空数据卷。

## API 接口

### 认证

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/auth/register` | 注册，Body: `{"username":"...", "password":"..."}` |
| POST | `/auth/login` | 登录，返回 JWT Token |

## API

### v2（推荐）

所有 v2 接口统一返回格式：

```json
{ "status": 0, "data": ..., "message": "" }
```

- `status`: `0` 成功 / `-1` 报错
- `data`: 所有响应数据
- `message`: 错误描述（成功时为空）

错误时同时返回适当的 HTTP 状态码（400/404/409/401/500 等）。

#### 认证（无需 Token）

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/v2/auth/login` | `{"username","password"}` → `{ data: { token } }` |
| POST | `/api/v2/auth/register` | `{"username","password","register_token?"}` → `{ data: { token, api_token } }` |
| GET | `/api/v2/auth/config` | → `{ data: { allow_register, register_need_token } }` |

#### 用户（Bearer Token）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/v2/me` | 获取用户信息 |
| PATCH | `/api/v2/me` | 更新资料 `{"nickname","avatar"}` |
| PUT | `/api/v2/me/password` | 修改密码 `{"old_password","new_password"}` |
| POST | `/api/v2/me/token` | 重新生成 API Token（创建新 Token + 全部权限）→ `{ data: { api_token } }` |

#### API Token 管理（Bearer Token）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/v2/tokens` | 列出所有 API Token |
| POST | `/api/v2/tokens` | 创建 Token `{"name","permissions"}` → `{ data: { id, name, raw_token, permissions } }` |
| PUT | `/api/v2/tokens/:id` | 更新 Token `{"name","permissions","is_active"}` |
| DELETE | `/api/v2/tokens/:id` | 删除/吊销 Token |
| GET | `/api/v2/tokens/permissions` | 获取权限标签列表（用于前端展示） |

权限值（bitmask，可按位组合）：

| 权限 | 值 | 说明 |
|------|-----|------|
| Read-only | `1<<0` (1) | 查看记录列表和详情 |
| Read-Write | `1<<1` (2) | 添加、修改、删除记录及更改状态 |
| View Personal Info | `1<<2` (4) | 查看用户昵称、头像等信息 |
| Modify Personal Info | `1<<3` (8) | 修改用户昵称、头像等信息 |
| Add Record | `1<<4` (16) | 添加新的追番记录 |
| Delete Record | `1<<5` (32) | 删除追番记录 |
| Modify Record | `1<<6` (64) | 修改追番进度 |
| Change Status | `1<<7` (128) | 更改追番状态 |
| Allow All | `u64::MAX` | 允许所有操作 |

> 例：`permissions = 1 | 4 | 16 = 21` 表示只读 + 查看个人信息 + 添加记录

#### 搜索（Bearer Token）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/v2/search?q=Re0&page=1` | 在线搜索 Bangumi（搜索页轻量，详情页 24h 缓存） |
| GET | `/api/v2/bangumi/:id?force=true` | 按 Bangumi ID 获取详情（24h 本地缓存，`?force=true` 跳过） |
| POST | `/api/v2/bangumi/:id/refresh` | 强制重新抓取并覆盖 Bangumi 的标题、类型、封面与详情缓存 |
| GET | `/api/v2/imdb/search?q=Interstellar&page=1&use_api=false` | 在线搜索 IMDb，支持免 API / OMDb API |
| GET | `/api/v2/imdb/:id?force=true&use_api=false` | 按 IMDb `tt...` ID 获取详情（24h 本地缓存） |
| GET | `/api/v2/search/local?q=...&page=1&page_size=20` | 搜索本地缓存 |

#### 追番记录（Bearer Token）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/v2/records` | 列出所有追番（基本信息） |
| GET | `/api/v2/records/detail` | 列出所有追番（含详情） |
| POST | `/api/v2/records` | 添加追踪 `{"bangumi_id"}` / `{"source":"imdb","external_id":"tt..."}` / `{"other_id"}` |
| GET | `/api/v2/records/bangumi/:id` | 按 Bangumi ID 获取进度 |
| PATCH | `/api/v2/records/bangumi/:id` | 按 Bangumi ID 更新进度 `{"recorder","user_status"}` |
| DELETE | `/api/v2/records/bangumi/:id?hard_delete=false` | 按 Bangumi ID 删除记录，默认软删除，`hard_delete=true` 为硬删除 |
| GET | `/api/v2/records/imdb/:id` | 按 IMDb ID 获取进度 |
| PATCH | `/api/v2/records/imdb/:id` | 按 IMDb ID 更新进度 `{"recorder","user_status"}` |
| DELETE | `/api/v2/records/imdb/:id?hard_delete=false` | 按 IMDb ID 删除记录，默认软删除，`hard_delete=true` 为硬删除 |
| GET | `/api/v2/records/custom/:id` | 按自定义条目 ID 获取进度 |
| DELETE | `/api/v2/records/custom/:id?hard_delete=false` | 按自定义条目 ID 删除，默认软删除，`hard_delete=true` 为硬删除 |
| DELETE | `/api/v2/records/recording/:id` | 按记录 ID 删除 |

#### 单集追踪（Bearer Token）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/v2/bangumi/:id/episodes` | 获取剧集元数据列表（爬取 bgm.tv 并缓存，24h TTL，`?force=true` 强制刷新） |
| GET | `/api/v2/records/bangumi/:id/episodes` | 获取单集追踪状态（含元数据合并，`?force=true` 强制刷新缓存） |
| PATCH | `/api/v2/records/bangumi/:id/episodes/:ordinal` | 更新单集进度 `{"watched","progress_seconds","duration_seconds"}`；若 Subject 从 74 等非 1 编号开始，兼容将季内 `1` 映射为真实 `74`，并在响应 `message` 返回 Warning |

更新单集时会自动同步主表 `recorder` 字段：集数 = max(ordinal where watched=1)，时间 = max(progress_seconds) 格式化为 mm:ss。

剧集元数据缓存以每次完整抓取的 `fetch_generation` 为快照边界。未出现在最新完整快照中的行先标记为 stale，连续两次完整抓取均缺失后才会删除；该清理仅作用于 `bangumi_episodes`，不会删除用户的 `episode_records`，并会把 Subject ID、清理数量和 ordinal 写入系统日志。完整缓存同样遵循 24 小时 TTL，空标题或空中文名视为不完整；`force=true` 会生成新快照并参与相同的安全清理流程。

#### 日志与设置（Bearer Token）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/v2/logs/recordings?page=1&page_size=50` | 查看当前用户的追踪记录日志，包含创建、删除、进度、状态和单集变更 |
| GET | `/api/v2/settings/auto-cleanup` | 获取当前用户的自动清理开关 |
| PUT | `/api/v2/settings/auto-cleanup` | 更新当前用户的自动清理开关 `{"enabled":true}` |

自动清理只作用于开启该开关的用户：服务启动时会执行一次，之后每天服务器本地时间 `00:00` 执行一次，删除 `is_delete=1` 且 `updated_at` 超过 30 天的记录。日志数据永远保留，不通过外键级联删除或改写。

#### 同步

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/v2/sync` | 批量同步（一轮往返，更新仲裁按较新时间戳胜出） |
| GET | `/api/v2/sync/incremental?since=` | 增量同步（返回指定时间戳后变更的记录） |
| POST | `/api/v2/sync_ani` | Animeko 游标式主记录 + 全量单集双向同步；写入前校验 ordinal 必须存在于当前剧集元数据 |

#### 开放接口（API Token 鉴权）

推荐使用 `Authorization: Bearer <api-token>`；`?token=xxx` 仅为旧客户端兼容保留。每个 Token
拥有独立的权限组合，不满足权限时返回 `403 Forbidden`。

| 方法 | 路径 | 所需权限 | 说明 |
|------|------|---------|------|
| GET | `/api/v2/open/me?token=` | View Personal Info | 用户信息 |
| GET | `/api/v2/open/records?token=` | Read-only / Read-Write | 列出追番 |
| GET | `/api/v2/open/records/detail?token=` | Read-only / Read-Write | 详细列表 |
| POST | `/api/v2/open/records?token=&bangumi_id=` | Add Record / Read-Write | 添加追番 |
| GET | `/api/v2/open/records/bangumi/:id?token=` | Read-only / Read-Write | 获取进度 |
| PATCH | `/api/v2/open/records/bangumi/:id?token=&recorder=` | Modify Record / Change Status / Read-Write | 更新进度 |
| DELETE | `/api/v2/open/records/bangumi/:id?token=&hard_delete=false` | Delete Record / Read-Write | 删除记录，支持硬删除 |
| GET | `/api/v2/open/records/custom/:id?token=` | Read-only / Read-Write | 获取自定义进度 |
| PATCH | `/api/v2/open/records/custom/:id?token=` | Modify Record / Change Status / Read-Write | 更新自定义进度 |
| DELETE | `/api/v2/open/records/custom/:id?token=` | Delete Record / Read-Write | 删除自定义记录 |
| GET | `/api/v2/open/search?q=Re0&token=` | Read-only / Read-Write | 在线搜索 |
| GET | `/api/v2/open/bangumi/:id?force=true&token=` | Read-only / Read-Write | 获取详情（24h 缓存） |
| GET | `/api/v2/open/imdb/search?q=Interstellar&use_api=false&token=` | Read-only / Read-Write | IMDb 搜索 |
| GET | `/api/v2/open/imdb/:id?force=true&use_api=false&token=` | Read-only / Read-Write | IMDb 详情 |
| GET | `/api/v2/open/other/:id?token=` | Read-only / Read-Write | 自定义条目详情 |
| GET | `/api/v2/open/records/imdb/:id?token=` | Read-only / Read-Write | 获取 IMDb 进度 |
| PATCH | `/api/v2/open/records/imdb/:id?token=` | Modify Record / Change Status / Read-Write | 更新 IMDb 进度 |
| DELETE | `/api/v2/open/records/imdb/:id?token=&hard_delete=false` | Delete Record / Read-Write | 删除 IMDb 记录，支持硬删除 |
| GET | `/api/v2/open/bangumi/:id/episodes?force=true&token=` | Read-only / Read-Write | 剧集元数据 |
| GET | `/api/v2/open/episodes/:bangumi_id?force=true&token=` | Read-only / Read-Write | 单集追踪列表 |
| PATCH | `/api/v2/open/episodes/:bangumi_id/:ordinal?token=` | Modify Record / Read-Write | 更新单集进度 |
| POST | `/api/v2/open/sync?token=` | Read-Write | 批量同步 |
| POST | `/api/v2/open/sync_ani?token=` | Read-only + Read-Write | 游标式主记录 + 单集同步（Animeko 兼容接口，写入年度总结日志） |
| GET | `/api/v2/open/sync/incremental?since=&token=` | Read-only / Read-Write | 增量同步 |
| GET | `/api/v2/open/search/local?q=...&token=` | Read-only / Read-Write | 本地搜索 |
| GET | `/api/v2/open/logs/recordings?token=` | Read Logs | 追踪记录日志，可用于导出和年度总结 |
| GET | `/api/v2/open/logs/system?token=` | Read Logs | 系统审计日志 |

### v1（向下兼容）

v1 接口保持 `/api/v1/*` 和 `/api/v1/open/*` 路径不变，响应格式为旧版自定义结构体，仅用于兼容已有客户端。`/api/v1/open/*` 同样支持 `Authorization: Bearer <api-token>`，并继续兼容 `?token=`。新项目请使用 v2。

完整 API 文档见 [OpenAPI.yaml](./OpenAPI.yaml)。

## 项目结构

```
├── src/                     # Rust 后端
│   ├── main.rs              # 入口、路由注册
│   ├── auth_bearer.rs       # JWT 认证、登录/注册
│   └── api/                 # API 路由处理
│       ├── search.rs        # Bangumi 搜索（网页抓取）
│       ├── imdb.rs          # IMDb 搜索/详情缓存（免 API / OMDb API）
│       ├── new.rs           # 添加追番
│       ├── list.rs          # 追番列表
│       ├── get_recorder.rs  # 获取进度
│       ├── update_recorder.rs # 更新进度
│       ├── logs.rs            # 记录日志、内部系统日志、自动清理设置和清理任务
│       ├── detail_list.rs   # 详细列表
│       ├── api_token.rs     # Token 鉴权 & 权限系统
│       ├── open/            # API Token 鉴权的开放接口（委托至 regular handler）
│       └── v2/              # v2 API（RESTful + 统一 {status,data,message} 格式）
│           └── token.rs     # API Token CRUD 管理
├── frontend/                # Next.js App Router 前端（静态导出到 dist）
│   ├── app/                 # Server Component 根布局、元数据、全局主题
│   ├── components/
│   │   ├── ui/              # shadcn/ui 风格的 Radix 基础组件
│   │   ├── common/          # 加载/错误/空状态、封面、页面标题
│   │   └── layout/          # 响应式应用外壳与导航
│   ├── features/            # 登录、片库、搜索、详情、日志、设置
│   ├── lib/
│   │   ├── api/             # 严格类型的 v2 API 客户端
│   │   ├── auth-context.tsx # JWT 会话与用户状态
│   │   └── validation.ts    # Zod 表单验证
│   └── tests/               # Vitest / Testing Library 配置
├── migrations/              # 数据库迁移文件
└── OpenAPI.yaml             # API 规范文档
```

## 许可证

MIT
