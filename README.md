# Tauri + React + Typescript

This template should help get you started developing with Tauri, React and Typescript in Vite.

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## 程序启动

### 环境要求

- [Bun](https://bun.sh/)（包管理器，项目使用 `bun.lock`）
- [Rust](https://www.rust-lang.org/tools/install) 工具链（Tauri 后端编译需要）
- Windows 需安装 [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)（Win11 已内置）

### 安装依赖

```bash
bun install
```

### 开发模式（启动桌面应用）

```bash
bun run tauri dev
```

该命令会先启动 Vite 前端开发服务器，再编译并打开 Tauri 桌面窗口，支持热更新。

### Jev 筛选定位兜底（可选）

采集任务的排序、发布时间和平台专属筛选仍先走内置文案定位。页面改版导致定位失败时，可让 TypeSafe Jev 从当前可见控件中选择目标，再由桌面端执行点击。启动桌面应用前在进程环境设置 `TYPESAFE_API_KEY` 即可启用；未设置时不发送模型请求，采集沿用原流程。密钥不要写入仓库或应用日志。

Jev 只接收平台 ID、目标筛选文案和当前页面可见控件的短文本（可能含页面展示的名称），不接收账号 Cookie 或采集到的接口响应。模型返回的控件必须属于本次页面快照且概率至少为 0.8；请求失败或页面变化时仍按原有定位兜底。此功能当前依赖 Windows WebView2 的脚本回读，其他系统不会调用 Jev。

### 仅启动前端（浏览器调试）

```bash
bun run dev
```

### 打包构建

```bash
bun run tauri build
```

构建产物（安装包 / 可执行文件）位于 `src-tauri/target/release/`。
