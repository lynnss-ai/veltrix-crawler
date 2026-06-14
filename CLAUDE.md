# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概览

veltrix-crawler 是抖音 / 小红书等平台的内容采集桌面应用(Tauri + React)。核心特点:**不逆向平台签名**——用 WebView2 打开真实登录页,注入脚本拦截页面自己发出的接口响应,适配器只负责把响应解析为统一模型,从而绕开 a_bogus / X-Bogus 等签名与风控。

## 常用命令

包管理器是 **Bun**(`bun.lock`),不要用 npm / yarn。

- `bun install` — 安装前端依赖
- `bun run tauri dev` — 开发模式:启动 Vite + 编译并打开桌面窗口(热更新)
- `bun run dev` — 仅前端(浏览器调试,无 Tauri API,invoke 会失败)
- `bun run tauri build` — 打包,产物在 `src-tauri/target/release/`
- `bun run build` — 前端构建,内含 `tsc` 类型检查
- `bunx tsc --noEmit` — 仅跑前端类型检查(改完 .tsx/.ts 必跑)
- `cargo check -p veltrix-crawler` — 桌面后端编译检查(改完 Rust 必跑)
- `cargo check --workspace` — 全 workspace(改了 `crates/core` 实体后跑,确保 server 也不挂)

无自动化测试(Rust 与前端都没有 test 套件)。改动靠 `cargo check` + `tsc` + 手动 `bun run tauri dev` 验证。

数据库默认本地 SQLite;设环境变量 `VELTRIX_DATABASE_URL=postgres://...` 切 PostgreSQL(含密码的连接串只走环境变量,不落配置文件)。同一套 SeaORM 实体跨 SQLite/PG 复用。

## 架构

### 三 crate workspace
- **`crates/core`**(veltrix-core)— 共享库,被桌面端和 server 同时复用:`config`(平台/数据库配置)、`db`(SeaORM 实体 + 建表)、`api`(Axum HTTP `/api/v1` + JWT)。
- **`crates/server`**(veltrix-server)— 可独立部署的 HTTP API 服务,复用 core。
- **`src-tauri`**(veltrix-crawler)— 桌面端:`adapter`(平台解析器)、`webview`(WebView2 池 + 拦截 + RPA 滚动)、`commands`(Tauri 命令)、`cookie`(账号池)、`media`(素材下载)、`cloud`(云端配对/WS)、`model`(跨平台统一模型)。

### 采集数据流(核心,改采集前先读懂)
1. `commands::run_task` 选该平台一个可用账号,后台 `spawn` 异步采集,命令立即返回。
2. `webview::pool` 复用该账号的 WebView2 窗口(**per-account 数据目录隔离** = 多账号互不串登录态),导航到搜索页,注入脚本 hook fetch/XHR。
3. 命中平台 `intercept_patterns` 的响应被拦截回传;`run_legacy_scroll` 边滚动边交给 adapter 解析、按去重 `content_id` 计数——**智能停止**:达目标数 / 连续到底(`STAGNANT_STOP`)/ 网络无响应(`NO_RESPONSE_STOP`)/ 手动停 即结束。计数排除库中已有的 content_id(重跑只数新增)。
4. adapter(`DouyinAdapter` / `XhsAdapter`,注册在 `lib.rs`)把响应解析为统一 `Content` / `Comment`,**只解析、不发请求**。
5. 边采边入库(`persist_collected`,on-conflict upsert 判重,更新点赞/评论等统计);采集主体结束转 `downloading_media` 态,后台**串行**下载素材(封面/头像/视频转音频,每条 3~10s 随机间隔,已成功的旧内容跳过),全部处理完才落 `completed`。

新增平台 = 加平台配置 + 实现 `PlatformAdapter` trait + 在 `lib.rs` 注册,不改调度/模型/上报。

### 进程内服务编排(`lib.rs` setup)
桌面启动时:加载配置 → 连数据库并建表(阻塞) → spawn HTTP API(`127.0.0.1:8787`,Desktop 模式,不挂配对/Redis)→ spawn 云端 WS 客户端(有 pc_token 则自动拉起)→ 注册适配器 → 建系统托盘(**关闭主窗口是隐藏到托盘,不退进程**)。

## 关键约定(不易从单文件看出)

- **前后端契约**:`src/lib/api.ts` 的 TS 接口(`TaskView` / `ContentView` 等)必须和 `src-tauri/src/commands/*` 里 `#[derive(Serialize)]` + `serde(rename_all="camelCase")` 的 struct 逐字段对应。改一边要同步另一边,否则字段静默变 undefined。
- **数据库迁移**:只用逻辑外键(字段关联,实体 `Relation` 留空),**禁物理 FK**。加字段 = 改 entity + 在 `crates/core/src/db/mod.rs::init_schema` 追加 `ALTER TABLE ... ADD COLUMN ... DEFAULT`(兼容旧库;新建库走 entity DDL,已存在的库走 ALTER)。
- **数据归属**:业务数据记 `owner`(用户名);用户有 `dataScope`(all/self),`list_*` 命令按 scope 过滤(self 只看自己)。配置类数据(平台/行业/提示词等)共用,不分归属。
- **桌面鉴权**:桌面端登录**不发 token**,登录态存前端 localStorage + 后端 `AppState.current_user`;JWT 仅用于对外 HTTP API。
- **任务状态机**:pending → running → downloading_media → completed(失败/手动停为 failed/cancelled)。**completed 算活跃、留在任务列表**,只有 failed/cancelled 进归档 tab。进度靠后端 `task-progress` 事件实时推送 + 前端 2s 轮询兜底(轮询条件必须含 running 与 downloading_media)。
- **平台配置是抓包起点**:`config/mod.rs` 的 `builtin_default` 里 `search_url_template` / `intercept_patterns` 只是开箱骨架,真实接口路径需本机 `bun run tauri dev` 抓包核对后调整(代码注释已标注)。
- **Tauri 命令注册**:每个新 `#[tauri::command]` 都要加进 `lib.rs` 的 `invoke_handler![]` 列表才能被前端 invoke。
