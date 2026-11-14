# NewPhoto

当前版本：`0.1.0-beta.1`（测试版）。

自托管 AI 图片工作台。Rust/Axum 服务、Leptos CSR/WASM 页面、PostgreSQL
持久队列和私有图片目录。支持本地登录、管理员/用户权限、真实生成/原图编辑、
任务与 HTTP 记录、图片下载、用户和模型管理、受限声明式入口插件。

默认中文；完整通用侧栏、管理员菜单、右上角明亮/暗黑/跟随系统主题、红色退出。
入口格式与实际模型配置分离。内置 `openai.images.generate`、`openai.images.edit`，
不预设特定最新模型，也不宣称仅更改地址就兼容所有服务。

## 预编译平台包

[GitHub Releases](https://github.com/AptS-2077/NewPhoto/releases) 提供 Windows x64、
Linux x64/ARM64、macOS Intel/Apple Silicon 平台包，版本以发布页实际附件为准。
每包含服务程序、已编译网页、空配置模板和运行说明，需要自行准备 PostgreSQL。
下载后核对 SHA256SUMS，再按包内 README 初始化自己的主密钥和首个管理员。
发布流水线固定读取原有版本标签，不把本地数据、密钥或验收资料装入包中。

## 生产部署

需要 Linux、Docker Engine/Compose、域名和指向服务器的 DNS；80/443 用于
Caddy 证书申请及 HTTPS。数据库与应用端口不映射公网。V1 仅允许一个服务实例。

```sh
cp .env.example .env
install -d -m 700 secrets
(umask 077; openssl rand -base64 32 > secrets/master.key)
chmod 444 secrets/master.key
```

编辑 `.env`：填写真实域名、HTTPS Origin、证书联系邮箱；生成随机数据库密码，
同时更新 POSTGRES_PASSWORD 和 DATABASE_URL（URL 中特殊字符需百分号编码）。
主密钥必须保留为 32 字节的 Base64 文件；外层 secrets 目录仅管理员可进入，
Compose 将文件只读挂载给非 root 应用用户。不要提交 `.env` 或 secrets。

```sh
docker compose build
docker compose up -d db app
docker compose run --rm app newphoto admin create --username admin
docker compose up -d https
docker compose logs -f app
```

CLI 隐藏输入首个管理员密码（12–128 字符，无固定默认密码）。已有管理员后，
后续账号从“用户管理”创建；没有公开注册或抢占初始化接口。启动自动执行 SQLx
迁移，也可显式执行 `docker compose run --rm app newphoto migrate`。
首次使用先在“模型管理”创建并启用模型，选择“写入/替换密钥”，再分配给普通用户。
普通用户默认 selected 空模型集合，必须主动授权。

就绪检查 `/health/ready` 验证数据库和存储；存活检查 `/health/live`。
Docker 构建固定 Rust 1.96.1、wasm-bindgen 0.2.130、Cargo.lock；镜像使用
明确版本标签，不使用 latest。升级前备份；发布时可进一步按部署平台锁定镜像 digest。
容器构建、真实公网证书和真实供应商出图须在目标环境验证；Mock 不代表真实上游验收。

## 本地开发（Windows）

需要 Rust 1.96.1（工具链文件自动安装）、Node.js 22+、Python 3.12+，
以及与锁文件一致的 wasm-bindgen CLI 0.2.130。无需系统级安装 PostgreSQL。

```powershell
npm ci
cargo install wasm-bindgen-cli --version 0.2.130 --locked
npm run build:web
npm run dev:db
```

保持数据库终端运行，在另一终端执行：

```powershell
.\scripts\dev-server.ps1 -CreateAdmin
.\scripts\dev-server.ps1
```

浏览器访问 `http://127.0.0.1:8080`。首次管理员密码交互输入；开发模式显式
允许 Secure=false，且只接受环回 Origin。开发数据库密码随机生成在被忽略的
`.cache/postgres-password`，连接串为 `.cache/database-url`；不要公开或提交。
停止时先 Ctrl+C 应用，再 Ctrl+C 数据库，保留 `.cache/postgres` 数据。
静态资源更新需重新 `npm run build:web` 并刷新浏览器。

## 配置

| 变量 | 用途 / 默认 |
|---|---|
| DATABASE_URL | 必填 PostgreSQL 连接串 |
| NEWPHOTO_MASTER_KEY_FILE | 主密钥文件，默认 secrets/master.key |
| NEWPHOTO_BIND | 默认 127.0.0.1:8080；容器 0.0.0.0:8080 |
| NEWPHOTO_PUBLIC_ORIGIN | 精确同源校验，默认 http://127.0.0.1:8080 |
| NEWPHOTO_COOKIE_SECURE | 默认 true；只允许显式本机开发 false |
| NEWPHOTO_DATA_DIR | 私有数据根目录，默认 data |
| NEWPHOTO_WEB_DIR | 默认 web/dist；容器 /app/web |
| NEWPHOTO_TASK_CONCURRENCY | 默认 2，范围 1–8 |
| NEWPHOTO_ALLOWED_UPSTREAMS | 内网例外 host:port 清单，逗号分隔；默认空 |
| NEWPHOTO_RESULT_HOSTS | 额外成果域名 host:port 清单；默认仅上游同源 |
| RUST_LOG | 默认 newphoto=info |

只默认允许公网 HTTPS。DNS 解析后的实际目标 IP 固定到请求客户端；拒绝私网、
环回、链路本地、云元数据、保留地址。内网例外由服务器配置，普通用户不能扩大。
成果下载独立客户端，不继承 API Authorization；跨域下载需明确 RESULT_HOSTS。
Base URL 使用带尾斜线前缀，入口仅相对路径，避免丢失或重复 /v1。
输入上限 25 MiB / 40 MP，响应体 40 MiB，图片处理限并发阻塞线程。

## 示例插件与故障测试

```powershell
python scripts/package-plugin.py
npm run mock
```

打包脚本会在本机生成 `examples/simple-image-1.0.0.npplugin`；后台安装该包，选“稍后刷新”验证 pending，
然后“刷新生效”。给 `example.simple-image.generate` 绑定模型：
地址 `http://127.0.0.1:19091/adapter/`、上游模型 `mock-url`、任意非敏感测试密钥，
启用并授权后生成。开发脚本已只放行测试用 19091/19092。
Mock 仅用于测试，不能当作真实服务实测。支持 mock-ok、mock-url、mock-http-404、
mock-http-500、mock-read-body、mock-empty、mock-conflict、mock-slow 等故障模型。

```powershell
cargo test --locked -p newphoto -j 2
node --check web/app.js
node tests/theme.mjs
python tests/extended.py
$env:NEWPHOTO_TEST_ADMIN_PASSWORD = '<本地测试管理员密码>'
python tests/acceptance.py
```

`tests/acceptance.py` 仅在本地隔离验收实例运行：会创建 Mock 模型和测试用户、
短暂注入本地存储故障，并重启 `.cache/server-pid` 指向的本次测试服务。
不要对生产数据库运行。结果写 docs/api-test-results.json，测试账号信息只写
被忽略的 .cache/acceptance-state.json；不包含用户上游密钥。

`tests/extended.py` 会在同一个本地开发 PostgreSQL 中临时创建独立数据库、
启动 18080 端口的服务并在结束后删除该测试数据库；不会修改主实例配置。
覆盖全新迁移、并发最后管理员保护、插件在途升级、凭据修改、会话过期和列表回归。
本机与容器均须空闲出相应端口。模型 defaults 仅接受已声明的 `size` 和固定 `count:1`，
网页在选择模型时应用默认尺寸；高级参数须由受验证的入口映射支持。

## 备份与恢复

数据库、data 图片/缩略图/插件/回执、主密钥都必须备份；数据库备份与主密钥分开保护。
先停止 app（保留数据库），再做一致备份。示例在 Linux Bash 下执行：

```sh
docker compose stop app
docker compose exec -T db pg_dump -U newphoto -Fc newphoto > newphoto-db.dump
docker compose run --rm -T --no-deps --entrypoint tar app -C /app/data -czf - . > newphoto-data.tgz
```

将 secrets/master.key 单独加密备份到不同位置，严禁把主密钥与数据库备份公开打包。
备份完成再 `docker compose start app`。保留 Compose 项目名与卷标识；
不要执行 `docker compose down -v`，该命令会销毁持久卷。

恢复到已启动的空 db 且 app 停止的环境：

```sh
docker compose exec -T db pg_restore -U newphoto -d newphoto --clean --if-exists < newphoto-db.dump
docker compose run --rm -T --no-deps --entrypoint tar app -C /app/data -xzf - < newphoto-data.tgz
docker compose run --rm --no-deps app newphoto recovery pause
docker compose up -d app https
```

先恢复原主密钥，再运行 pause；必须在 app 启动之前完成。新提交与队列外部调用
暂停，完整回执可继续本地恢复。管理员核对 submitting/submission_unknown 与供应商
记录，禁止把所有旧任务重置为 queued 或重放生成。确认完毕才执行
`docker compose exec app newphoto recovery resume`。没有主密钥则无法恢复加密凭据。

## 文档与目录

- `server/`：Axum API、会话/权限、任务 Worker、存储、插件解释器、数据库迁移。
- `web/`：Leptos CSR 页面结构与角色渲染，ES modules 负责同源 API/交互，原创 CSS。
- `schemas/`、`examples/`：版本化插件 Schema、打包源文件、golden fixture、Mock 服务。
- `docs/api.md`：API 契约；`docs/ui-reference.md`：固定 AsterDrive 参考提交。
- 本地验收报告、`docs/*-test-results.json` 和 `docs/screenshots/` 仅保留在本机，不随源码发布。

仓库仅发布源代码、锁文件、脱敏配置示例和构建/测试/部署说明；不包含 `.env`、
密钥、数据库、用户图片、日志、插件打包产物、依赖目录或编译产物。

V1 预留 AuthProvider 与独立 feature 边界。暂不执行 WASM 插件/任意脚本、
异步厂商协议、蒙版画笔、支付、团队空间、公开分享、Redis 或多副本。
MIT License。没有复制 AsterDrive 源代码/图片；仅按固定版本的交互和视觉原则原创实现。
