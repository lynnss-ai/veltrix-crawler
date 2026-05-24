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

### 仅启动前端（浏览器调试）

```bash
bun run dev
```

### 打包构建

```bash
bun run tauri build
```

构建产物（安装包 / 可执行文件）位于 `src-tauri/target/release/`。
