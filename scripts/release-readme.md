# NewPhoto 预编译平台包

本包包含 NewPhoto 服务程序、已编译的网页、许可证、API 说明和空配置模板。
版本、目标架构、原始源码提交与文件 SHA-256 记录在 BUILD-INFO.json。
网页在浏览器中运行，服务进程通过 PostgreSQL 保存账号和任务。

## 首次启动

1. 安装并启动独立的 PostgreSQL（建议 18），创建专用账号和数据库。
   本包不包含数据库、账号、默认密码、用户图片、上游 API 密钥或主密钥。
2. 保持整个目录结构，进入解压后的目录。复制 `.env.example` 为 `.env`，
   填写 `DATABASE_URL`。连接串格式为 `postgresql://用户:密码@主机:5432/数据库`；
   保留连接串中的真实凭据仅在本机，特殊字符应做 URL 编码。
3. Linux/macOS 执行 `./newphoto key generate`，Windows PowerShell 执行
   `.\newphoto.exe key generate`。此命令创建专属 `secrets/master.key`，不会覆盖已有密钥。
   主密钥须与数据库分别备份；Windows 上请限制 secrets 目录的 NTFS 权限。
4. 执行 `./newphoto migrate`、`./newphoto admin create --username admin`；
   Windows 对应使用 `.\newphoto.exe`。按提示输入自己的管理员密码。
5. 执行 `./newphoto serve`（Windows：`.\newphoto.exe serve`），浏览器访问
   `http://127.0.0.1:8080`。首次登录后在管理后台配置并授权真实模型。

默认模板只适用于本机 HTTP 试用（Secure=false 且绑定回环地址）。服务器部署时配置
HTTPS 反向代理，将 PUBLIC_ORIGIN 改为自己的 HTTPS Origin，并设置 COOKIE_SECURE=true。
不要把本机试用配置直接暴露公网。环境变量完整名称以 `.env.example` 为准。

## 平台与验证

- Windows 包：64 位 x86，Windows 10/11 或兼容的 Windows Server；可能需要
  Microsoft Visual C++ 2015–2022 x64 运行库。本程序尚未做 Windows 代码签名。
- Linux x86_64 包：Ubuntu 22.04 / glibc 2.35 或兼容系统；ARM64 包：Ubuntu 24.04 /
  glibc 2.39 或兼容系统。需要系统 CA 证书；不适用于原生 Alpine/musl。
- macOS 包：分别为 Intel x86_64 与 Apple Silicon arm64，建议 macOS 14 或更新版本；
  尚未进行 Apple 签名或公证，系统可能要求在安全设置中确认打开。

每个平台在对应原生构建环境执行 Rust 单元测试及 CLI 版本、帮助和主密钥生成检查。
这不等同于已验证真实模型兼容性、公网 HTTPS 部署或所有目标操作系统版本。
请先核对下载页 SHA256SUMS；包内不会携带测试时生成的密钥。

使用时始终在解压目录中启动；网页路径默认为 `web/dist`。备份须包括 PostgreSQL、
运行产生的 `data/` 和单独保护的主密钥。恢复旧备份后先执行 `newphoto recovery pause`，
核对可能已提交的任务，再执行 `newphoto recovery resume`，避免重复生成。

源码与完整部署说明：https://github.com/AptS-2077/NewPhoto
