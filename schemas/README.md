# NewPhoto 声明式插件

包内 `manifest.json` 与入口定义分别遵循 `manifest-v1.json` 和 `entry-definition-v1.json`。
图生图能力按入口声明。编辑入口在清单中同时提供
`"capabilities":["image.input"]` 和
`"image_input":{"max_count":4,"max_bytes":10485760,"accepts_weights":true}`，
并在入口定义的 `request.reference_images` 中提供编码和字段映射。
使用 `operation:"edit"` 与 `input_slots:["image"]`。

| `reference_images.encoding` | 上游请求中的字段 |
|---|---|
| `url` | JSON 字符串数组，每项为一个短期签名的 HTTPS 参考图地址 |
| `base64` | JSON 字符串数组，每项为一张图片的标准 Base64，原始图片合计上限为 25 MiB |
| `multipart` | 每张图作为同一个字段名的独立文件 part，支持 `image[]` 风格字段名 |

字段名由 `reference_images.field` 指定。支持权重时，通过 `weights_field` 指定对应的权重数组字段，
multipart 采用 JSON 数组文本。Base64 或 multipart 可设置 `flatten_alpha:true`，把透明图片合成为白底 PNG。
图片与权重顺序始终一致。JSON 响应可用 `response.usage_pointer` 指向 token 用量对象。
插件无法读取本机路径、会话凭据或其他账号的资产。

[示例图生图插件](../examples/image-to-image/manifest.json) 配合 `npm run mock` 使用，
模型的 API Base URL 为 `http://127.0.0.1:19091/v1/`，上游模型名为 `mock-ok`。
本地服务需要在 `NEWPHOTO_ALLOWED_UPSTREAMS` 中明确允许 `127.0.0.1:19091`。
打包时将示例中的 `manifest.json` 和 `entries/edit.json` 放入 ZIP，扩展名改为 `.npplugin`，
然后在管理后台安装、发布并绑定模型。Golden fixture 包含 Mock 校验的提示词、权重与用量。
