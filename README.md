# VeltrixLoop

VeltrixLoop 是面向内容运营的本地优先桌面应用：从真实平台采集内容、评论和作者数据，沉淀转写与意向洞察，再辅助内容生产和多账号发布。项目采用 Tauri 2、React 19、Rust 和 SQLite/PostgreSQL。

## 产品边界

- **数据洞察**：平台采集、账号池、内容库、评论库、作者画像、转写、封面 OCR 和评论意向分析。
- **内容生产**：默认进入 AI 成片工作台，先定义平台、目的、时长和节奏；DeepSeek-V4.1-Flash 根据真实镜头抽帧选择初剪片段，本地 FFmpeg 完成竖版导出。原有多轨时间线只保留为高级调整，不再作为主产品方向扩展。
- **发布服务**：客户发布账号池已可用；自动发布流程仍在建设中。当前成片导出不等于自动发布。

AI 成片当前主要理解**画面截图**，尚没有口播的句子级时间戳，因此不能保证口播观点连贯，也不能仅凭模型输出认定商品功效或内容权利。成片需要人工复核。若 DeepSeek 未配置，可在镜头识别成功后显式选择不依赖模型的本地粗剪。

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

DeepSeek 厂商和密钥在应用的模型设置中配置。AI 成片使用官方模型标识 `deepseek-flash`，密钥只由 Rust 侧读取，不传给前端。日常验证请用 `bunx tsc --noEmit`、`cargo check -p veltrix-crawler` 和 `cargo test -p veltrix-crawler --lib`；`bun run tauri build` 会自动递增补丁版本号，不适合只做检查时运行。

详细仓库约定见 [AGENTS.md](AGENTS.md)。
