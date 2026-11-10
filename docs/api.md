# NewPhoto V1 API

业务前缀 `/api/v1`；健康检查为根路径 `/health/live`、`/health/ready`。
同源 Cookie 会话；除登录外的写请求还需 `Origin` 和从 `/auth/me` 得到的
`X-CSRF-Token`。登录也校验 Origin。默认会话绝对 8 小时、空闲 2 小时。
所有权限、角色、模型授权均在每次请求重新读取，不相信客户端声明。

## 路由

| 方法 | 路径 | 权限与行为 |
|---|---|---|
| POST | /auth/login | `{username,password}`；限流；返回 user、csrf_token，设置 HttpOnly Cookie |
| GET | /auth/me | 当前 user 与 csrf_token |
| POST | /auth/logout | 撤销当前会话，清 Cookie |
| GET/PATCH | /me | GET 同当前身份；PATCH 仅 `{display_name,revision}` |
| GET | /entry-formats | `{registry_revision,items}`；仅已发布入口，两个内置格式始终存在 |
| GET | /models?entry_format_id=… | 获授权且启用的配置；不含地址、上游密钥 |
| POST | /assets/inputs | multipart 单个 `image`；返回资产 ID、格式、尺寸 |
| POST | /tasks | 必须带 `Idempotency-Key`，见下文；返回 202 |
| GET | /tasks | 本人任务；管理员可显式 `all_users=true`，留审计 |
| GET | /tasks/{id} | `{task,server_now}`；真实时间、状态、最新上游摘要与资产 |
| GET | /tasks/{id}/upstream-requests | 各阶段脱敏 HTTP 记录 |
| GET | /images | 生成结果；与任务相同的读取规则 |
| GET | /assets/{id}/preview?thumb=true | 授权图片/缩略图；禁止静态旁路 |
| GET | /assets/{id}/download | 下载权限；正确图片 Content-Type、attachment 文件名 |
| GET/POST | /admin/users | 管理员列表/创建用户 |
| GET/PATCH/DELETE | /admin/users/{id} | 详情、原子修改授权、软删除 |
| POST | /admin/users/{id}/reset-password | `{password,revision}`；撤销目标所有会话 |
| GET/POST | /admin/models | 管理员列表/创建模型 |
| GET/PATCH/DELETE | /admin/models/{id} | 详情、修改、软删除；密钥仅 has_secret 和掩码 |
| POST | /admin/models/{id}/test | 与创建任务同结构，加 `confirm_cost:true`，正常任务链路 |
| GET | /admin/plugins | 包信息、期望/生效状态、注册表版本 |
| POST | /admin/plugins/install | multipart `package`，无认证入口另需 `approve_no_auth=true` |
| GET/PATCH/DELETE | /admin/plugins/{id} | 详情、允许配置修改、无引用时软移除 |
| POST | /admin/registry/refresh | 指定变更原子发布；见下文 |

普通用户读他人资源 404；缺少功能权限 403；无有效会话 401。
管理员读取他人记录或全站列表写审计。管理写操作需 revision。

## 列表与错误

列表接受 `page`（默认 1）、`per_page`（默认 20，最大 100）、`search`。
返回 `items,page,per_page,has_more`。入口发现接口返回完整的小型注册表快照。
错误按真实 HTTP 状态返回，例如：

```json
{"error":{"code":"PERMISSION_DENIED","message":"当前账号没有执行此操作的权限","request_id":"req_…","field_errors":[]}}
```

错误类别包括 UNAUTHENTICATED、PERMISSION_DENIED、NOT_FOUND、VALIDATION_ERROR、
MODEL_UNAVAILABLE、REGISTRY_CHANGED、PLUGIN_IN_USE、UPSTREAM_HTTP_ERROR、
SUBMISSION_UNKNOWN、RESULT_UNAVAILABLE。任务详情的 `error_code` 保存终态机器错误码，上游失败在通信记录中独立保存；
本地创建任务 202 不表示上游 202。

## 创建任务

```json
{
  "entry_format_id":"openai.images.edit",
  "model_config_id":"<UUID>",
  "model_revision":1,
  "registry_revision":1,
  "prompt":"保留主体，将背景改为海边",
  "input_asset_ids":["<本人输入资产 UUID>"],
  "output":{"size":"1024x1024","count":1}
}
```

尺寸以模型 capabilities.sizes 为准。generate 不接受原图；edit 恰好一张。
相同用户同键同请求回放原任务；同键不同内容 409；回放优先于新模型版本校验。
再次生成用新的 key，可带 `parent_task_id`，可能再次计费。

任务状态：queued → preparing → submitting → saving → succeeded；失败为 failed、
submission_unknown 或 result_unavailable。可能已经提交时禁止自动重发；
已有完整持久回执只恢复解码/下载/入库。未提交任务在权限/账号/模型/密钥
失效后终止。浏览器前台每 2 秒轮询、后台每 10 秒、终态停止。

## 用户与模型写入

用户：username（新建后不可改）、display_name、password（仅新建）、role
（admin/user）、status（enabled/disabled）、permissions、model_access
（selected/all_enabled）、model_ids。生成和下载都依赖 task.read_own。
权限和模型授权在同一事务保存。最后管理员由事务锁保护；禁止当前会话自降级、
自停用、自删除。删除保留历史归属。

模型：display_name、entry_format_id、upstream_model、api_base_url、
capabilities（sizes、input_mimes、max_input_bytes、max_pixels）、defaults、enabled。
credential_action 明确为 keep/replace/clear；replace 同时提交 credential。
defaults 只接受 `size`（须在 capabilities.sizes 内）和 `count:1`；其他字段明确拒绝。
密钥 AES-256-GCM 加密。普通更新影响新任务；任务保留非敏感快照。

## 发布注册表

```json
{"expected_registry_revision":1,"request_id":"<unique id>","changes":[{"id":"<installation UUID>","revision":1}]}
```

只发布指定项。候选快照全部校验成功后提交持久指针并更新内存快照。
相同 request_id 同内容幂等；版本冲突 409 REGISTRY_CHANGED；失败保持旧快照。
installed_pending 跨 F5、登录、服务重启保持，不自动激活。
包 ID+版本+摘要不可变；同版本不同摘要 409；旧任务固定包摘要与注册表 revision。

定义格式见 `schemas/manifest-v1.json`、`schemas/entry-definition-v1.json`。
仅允许受限字段来源、结构化常量、multipart 资产槽、RFC 6901 响应映射，
恰好一个 URL/Base64/二进制成果；无脚本、任意文件、远程 Schema 或私网旁路。
