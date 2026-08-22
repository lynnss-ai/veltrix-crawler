# AGENTS.md

本文件为 AI 编码代理提供本仓库的工作指南。读者默认对项目零了解,请先通读「关键约定」一节再动代码。

## 项目概览

veltrix-crawler 是抖音 / 小红书 / 快手 / Bilibili / TikTok / YouTube 等平台的内容采集桌面应用(Tauri 2 + React 19)。核心特点:**不逆向平台签名**——用系统 WebView(Windows 为 WebView2)打开真实登录页,注入脚本 hook fetch/XHR 拦截页面自己发出的接口响应,适配器只负责把响应解析为统一模型,从而绕开 a_bogus / X-Bogus 等签名与风控。

除采集外,应用还包含:账号池(Cookie 管理)、评论意向分析、语音转写(「AI 文案提取」)、Obsidian 同步、LLM 对话(多家 OpenAI 兼容厂商)、桌面操作 Agent(编程 / RPA / 电脑操作)、云端配对远程控制。

桌面端窗口标题为 **VeltrixLoop**,Tauri identifier 为 `com.lynns.veltrix-crawler`。

## 技术栈

- **前端**:React 19 + TypeScript + Vite 7 + Tailwind CSS 4 + shadcn/ui(`components.json`,style `radix-nova`,图标 lucide)。包管理器 **Bun**(`bun.lock`),不要用 npm / yarn。
- **桌面端**:Tauri 2(Rust),依赖 WebView2(Win11 内置)。启用了 `unstable` feature(多 webview,`Window::add_child`),改动需回归验证采集窗口。
- **后端 Rust workspace** 三 crate(见下),SeaORM 1 + tokio + axum 0.8 + reqwest(rustls,不引 openssl)。
- **数据库**:默认本地 SQLite;设环境变量 `VELTRIX_DATABASE_URL=postgres://...` 切 PostgreSQL(含密码的连接串只走环境变量,不落配置文件)。同一套 SeaORM 实体跨 SQLite/PG 复用。
- **云模式**:`VELTRIX_MODE=cloud` 时 veltrix-server 绑 `0.0.0.0:8787` 并依赖 Redis(`VELTRIX_REDIS_URL`,默认 `redis://127.0.0.1:6379`);desktop 模式绑 `127.0.0.1:8787`,不依赖 Redis。

## 常用命令

- `bun install` — 安装前端依赖
- `bun run tauri dev` — 开发模式:启动 Vite + 编译并打开桌面窗口(热更新)
- `bun run dev` — 仅前端(浏览器调试,无 Tauri API,invoke 会失败)
- `bun run tauri build` — 打包,产物在 `src-tauri/target/release/`(捆绑资源含 `src-tauri/resources/ffmpeg.exe`)
- `bun run build` — 前端构建,内含 `tsc` 类型检查
- `bunx tsc --noEmit` — 仅跑前端类型检查(改完 .tsx/.ts 必跑)
- `cargo check -p veltrix-crawler` — 桌面后端编译检查(改完 Rust 必跑)
- `cargo check --workspace` — 全 workspace(改了 `crates/core` 实体后跑,确保 server 也不挂)

**无自动化测试**(Rust 与前端都没有 test 套件),也无 CI 配置。改动靠 `cargo check` + `tsc` + 手动 `bun run tauri dev` 验证。

构建辅助:`.cargo/config.toml` 在 Windows 下用 `rust-lld.exe` 替代 MSVC link.exe 加速增量链接;根 `Cargo.toml` 的 dev profile 为 `debug = "line-tables-only"`(保留行号级调试信息,加速 codegen 与链接)。

## 仓库结构与模块划分

```
crates/core      veltrix-core   — 共享库,被桌面端和 server 同时复用,不依赖 Tauri:
                                  config(平台/数据库配置)、db(SeaORM 实体 + 建表)、
                                  api(Axum HTTP /api/v1 + JWT + WS hub + 配对)、error
crates/server    veltrix-server — 可独立部署的 HTTP API 服务二进制,复用 core;
                                  部署形态由 VELTRIX_MODE 决定(cloud / desktop)
src-tauri        veltrix-crawler— 桌面端(bin + lib):
  adapter/       平台解析器(douyin/xhs/kuaishou/bilibili/tiktok/youtube)
  webview/       WebView 池(pool)、原生网络拦截(native_intercept)、脚本注入、cookie
  commands/      Tauri 命令(task / collect / dashboard / admin / billing / cloud / creation)
  agent/         桌面操作 Agent(chat / coding / computer / rpa / shell / ocr / uia / orchestrator …)
  cookie/        账号池;media/ 素材下载;model/ 跨平台统一模型
  llm/           LLM 对话(chat / embedding / intent / speech / provider)
  cloud/         云端配对 / WebSocket 客户端 / 远程执行
  obsidian/      Obsidian 同步;sandbox/ 编程本地沙盒(Job Object / killpg)
  lib.rs         进程编排入口:setup、系统托盘、invoke_handler! 命令注册
src/             前端:
  pages/         各页面(采集、内容库、评论库、账号、设置、对话、Agent 等)
  components/    业务组件 + ui/(shadcn 组件,勿手改生成件)
  lib/api.ts     前端与 Rust 后端的 invoke 契约层
index.html + recording-overlay.html — Vite 多页入口(主应用 + 录屏悬浮条轻量入口)
docs/            设计文档(agent-platform-design.md 等)
src-tauri/capabilities/ Tauri 权限:采集 WebView(veltrix-*)显式授权远程平台域名 invoke
```

数据库实体在 `crates/core/src/db/entity/`(account、content、comment、task、collect_record、chat_* 等 23 张表)。

## 采集数据流(核心,改采集前先读懂)

1. `commands::run_task` 选该平台一个可用账号,后台 `spawn` 异步采集,命令立即返回。
2. `webview::pool` 复用该账号的 WebView 窗口(**per-account 数据目录隔离** = 多账号互不串登录态),导航到搜索页,注入脚本 hook fetch/XHR。
3. 命中平台 `intercept_patterns` 的响应被拦截回传;`run_legacy_scroll` 边滚动边交给 adapter 解析、按去重 `content_id` 计数——**智能停止**:达目标数 / 连续到底 / 网络无响应 / 手动停 即结束。计数排除库中已有 content_id;**去重跳过**:本任务已采 ∪ 去重台账 `collect_records`(同平台、近 90 天)的内容整体跳过,删单条内容不清台账,「清空业务数据」连带清台账。
4. adapter(`DouyinAdapter` / `XhsAdapter` 等,注册在 `lib.rs`)把响应解析为统一 `Content` / `Comment`,**只解析、不发请求**。
5. 边采边入库(on-conflict upsert)。阶段顺序:内容采集 → 作者画像补采 → 评论采集 → 直链补取(开「音频提取」时;刻意排在评论后)→ 素材下载(并发 15 路;**采集窗口保活、账号锁延后到下载结束才释放**——每个并发批从存活窗口取一次轮换后的新会话 Cookie,用户关窗即终止)→ 关窗放锁 → 语音转写 → 评论意向分析 → Obsidian 同步 → 落 `completed`。

**新增平台** = 加平台配置 + 实现 `PlatformAdapter` trait + 在 `lib.rs` 注册,不改调度/模型/上报。

桌面启动编排(`lib.rs` setup):加载配置 → 连库建表(阻塞)→ spawn 内嵌 HTTP API(`127.0.0.1:8787`)→ spawn 云端 WS 客户端(有 pc_token 则自动拉起)→ 注册适配器 → 建系统托盘(**关闭主窗口是隐藏到托盘,不退进程**)。

## 关键约定(不易从单文件看出)

- **前后端契约**:`src/lib/api.ts` 的 TS 接口(`TaskView` / `ContentView` 等)必须和 `src-tauri/src/commands/*` 里 `#[derive(Serialize)]` + `serde(rename_all="camelCase")` 的 struct 逐字段对应。改一边要同步另一边,否则字段静默变 undefined。
- **数据库迁移**:只用逻辑外键(字段关联,实体 `Relation` 留空),**禁物理 FK**。加字段 = 改 entity + 在 `crates/core/src/db/mod.rs::init_schema` 追加 `ALTER TABLE ... ADD COLUMN ... DEFAULT`(兼容旧库;新建库走 entity DDL,已存在的库走 ALTER)。
- **数据归属**:业务数据记 `owner`(用户名);用户有 `dataScope`(all/self),`list_*` 命令按 scope 过滤。配置类数据(平台/行业/提示词等)共用,不分归属。
- **桌面鉴权**:桌面端登录**不发 token**,登录态存前端 localStorage + 后端 `AppState.current_user`;JWT 仅用于对外 HTTP API(`/api/v1`)。密码哈希用 argon2。
- **任务状态机**:pending → running → downloading_media → completed(失败/手动停为 failed/cancelled)。**completed 算活跃、留在任务列表**,只有 failed/cancelled 进归档 tab。进度靠后端 `task-progress` 事件实时推送 + 前端 2s 轮询兜底(轮询条件必须含 running 与 downloading_media)。
- **平台配置是抓包起点**:`crates/core/src/config/mod.rs` 的 `builtin_default` 里 `search_url_template` / `intercept_patterns` 只是开箱骨架,真实接口路径需本机 `bun run tauri dev` 抓包核对后调整(代码注释已标注)。
- **Tauri 命令注册**:每个新 `#[tauri::command]` 都要加进 `lib.rs` 的 `invoke_handler![]` 列表才能被前端 invoke。
- **采集 WebView 的远程权限**:`src-tauri/capabilities/collect-remote.json` 显式授权小红书/抖音/快手域名 invoke(回传拦截响应与 RPA 结果),新增平台域名要同步加这里。

## 代码风格

- 代码注释与文档统一使用**中文**;注释解释「为什么」而非「做什么」(如 Cargo.toml 中依赖旁的设计权衡注释)。
- Rust:错误处理 anyhow(应用层)/ thiserror(库层);日志用 `tracing`(tracing-appender 滚动落盘);异步 trait 用 `async-trait`(dyn 安全)。
- 前端:路径别名 `@` → `src/`;UI 用 shadcn/ui + Tailwind;日期用 date-fns;图标用 lucide-react。
- 遵循项目规范:函数参数 ≤ 4 个(多了封装为结构体,见 `docs/agent-platform-design.md` 中的示例)。

## 安全注意事项

- 数据库连接串、API key 等敏感信息只走环境变量(`VELTRIX_DATABASE_URL` / `VELTRIX_REDIS_URL`),不写入配置文件。
- 平台 Cookie / 登录态按账号隔离存放,不要在日志中打印。
- 主窗口 CSP 为 null、assetProtocol scope 为 `**`(本地素材展示需要);采集 WebView 加载外部平台页面,权限按 capabilities 最小授权,**不要给 `veltrix-*` 窗口加窗口控制类权限**。
- Tauri updater 验签公钥在 `tauri.conf.json`(当前为占位符,发布前需替换)。

## 部署

- 桌面端:`bun run tauri build`,产物在 `src-tauri/target/release/`(NSIS 安装包,含 `installer-hooks.nsh` 钩子)。
- 服务端:`cargo build -p veltrix-server --release`,通过 `VELTRIX_MODE` / `VELTRIX_DATA_DIR` / `VELTRIX_DATABASE_URL` / `VELTRIX_REDIS_URL` 环境变量配置。
