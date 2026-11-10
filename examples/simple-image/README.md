# Simple Image Adapter

受限声明式入口示例。使用 scripts/package-plugin.py 打包后，从管理员后台安装。
选择“稍后刷新”不会激活；选择“立即刷新生效”只发布当前插件。

Mock 服务：node examples/mock-server.mjs。
模型地址前缀：http://127.0.0.1:19091/adapter/，上游模型 mock-url 或 mock-ok。
服务端必须明确设置 NEWPHOTO_ALLOWED_UPSTREAMS=127.0.0.1:19091。
可填测试用非敏感密钥；本包不能读取该密钥，认证由宿主注入。
这不是生产模型或真实上游验收结果。
