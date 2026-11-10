# 界面参考记录

2026-11-04 编码前固定 AsterCommunity/AsterDrive 提交：
`41467b7e74ac8bc23b7c0ff87938a7dc70a9d733`。

已读取该提交的 frontend-panel/README.md 与
frontend-panel/frontend-uiux-guidelines.md。官方截图路径为
assets/Readme/Screenshot-English.webp；2026-11-04 已经 GitHub API 取得并实际查看该提交截图：浅中性背景、固定左导航、顶部工具条、窄边框和清楚的分组。不复制参考代码或图片，
不采用其 React 栈、云盘业务与工程指令。

| 部分 | 参考原则 | NewPhoto 实现 |
|---|---|---|
| 导航 | 稳定侧栏、工作台、移动端可见入口 | 232px 侧栏、768px 以下抽屉、完整通用菜单 |
| 表单 | label、草稿/保存、pending guard、错误反馈 | 语义表单、提交门闩、字段与请求错误 |
| 表格 | 紧凑、列表优先、行内可见操作 | 搜索分页、独立滚动容器、对象操作列 |
| 对话框 | 固定头尾、明确确认 | 原生 dialog、焦点、Escape、含对象名的确认 |
| 主题 | 中性背景、边框、对比度、focus | 三态主题、首绘前应用、红色退出 |

尺寸采用 NewPhoto 规范建议，不声称测量自 AsterDrive。样式为本项目原创。
动效遵守 prefers-reduced-motion；参考规范中的 React、bun 指令不适用。
