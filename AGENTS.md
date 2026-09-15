# AGENTS.md

本文件为 AI 编码代理提供本仓库的工作指南。读者默认对项目零了解,请先通读「关键约定」一节再动代码。

## 项目概览

veltrix-crawler 是抖音 / 小红书 / 快手 / Bilibili / TikTok / YouTube 等平台的内容采集桌面应用(Tauri 2 + React 19)。核心特点:**不逆向平台签名**——用系统 WebView(Windows 为 WebView2)打开真实登录页,注入脚本 hook fetch/XHR 拦截页面自己发出的接口响应,适配器只负责把响应解析为统一模型,从而绕开 a_bogus / X-Bogus 等签名与风控。

除采集外,应用还包含:账号池(Cookie 管理)、评论意向分析、语音转写(「AI 文案提取」)、Obsidian 同步、LLM 对话(多家 OpenAI 兼容厂商)、桌面操作 Agent(编程 / RPA / 电脑操作)、云端配对远程控制、发布服务(独立分类账号池 + 自动发布,建设中——账号池已可用,发布流程在第 1 期)。

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
- `bun run tauri build` — 打包;执行前自动递增补丁版本号并同步 `package.json` / `src-tauri/tauri.conf.json` / `src-tauri/Cargo.toml`,产物在 `target/release/`(捆绑资源含 `src-tauri/resources/ffmpeg.exe`)
- `bun run build` — 前端构建,内含 `tsc` 类型检查
- `bunx tsc --noEmit` — 仅跑前端类型检查(改完 .tsx/.ts 必跑)
- `cargo check -p veltrix-crawler` — 桌面后端编译检查(改完 Rust 必跑)
- `cargo check --workspace` — 全 workspace(改了 `crates/core` 实体后跑,确保 server 也不挂)

**无自动化测试**(Rust 与前端都没有 test 套件),也无 CI 配置。改动靠 `cargo check` + `tsc` + 手动 `bun run tauri dev` 验证。

构建辅助:`.cargo/config.toml` 在 Windows 下用 `rust-lld.exe` 替代 MSVC link.exe 加速增量链接;根 `Cargo.toml` 的 dev profile 为 `debug = "line-tables-only"`(保留行号级调试信息,加速 codegen 与链接)。

**OpenCV 构建环境(智能剪辑-场景检测,仅 Windows)**:预编译包在 `third_party/opencv/`(不进 git,首次需下载 opencv-4.10.0-windows.exe 自解压);`.cargo/config.toml [env]` 已配 `OPENCV_*`。绑定生成依赖 clang 工具链(全部走 scripts/.venv 的 pip 包,无需管理员):`CLANG_PATH` 指向 `third_party/clang-shim/clang.cmd`(ziglang 的 zig cc -target x86_64-windows-msvc 充当 clang 驱动;**`ziglang/lib/include` 已替换为 llvmorg-18.1.1 的 clang 内建头**,与 pip libclang 18.1.1 版本对齐,源包抽自 `third_party/llvm-project-llvmorg-18.1.1/`);`LIBCLANG_PATH` 指向 libclang pip 包,链接期 `libclang.lib` 由 pefile 导符号 + `zig dlltool` 生成(已放 libclang.dll 同目录,dll 另拷 target/debug 与 deps 供构建脚本加载);`OPENCV_CLANG_ARGS=-D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH`(pip libclang 18 过本机 VS 18 STL 的 clang≥20 版本检查)。运行时依赖 `opencv_world4100.dll` 等 3 个 DLL(dev 已拷 target/debug;打包走 `src-tauri/resources/`,tauri.conf bundle.resources 已含)。

**FFmpeg libav 集成(视频剪辑-探测,渐进迁移中)**:`.cargo/config.toml [env]` 配 `FFMPEG_DIR` 指向 `third_party/ffmpeg/`(不进 git;BtbN **win64-gpl-shared 9.0** 构建,含 include/ 与 MSVC 导入库)。依赖 `ffmpeg-next` 9.0(关默认 feature,显式挑 format/codec/filter/device/software-resampling/software-scaling;**勿开 `resampling`**,它映射到 FFmpeg 5 已移除的 avresample)。根 Cargo.toml `[patch.crates-io]` 指向本地 `third_party/ffmpeg-sys-next/`(唯一改动:bindgen 去掉 `runtime` feature——runtime 模式经 cargo 特性统一会传染 clang-sys,令 OpenCV 绑定生成器 panic;升级 ffmpeg-sys-next 需重新同步补丁)。**隐式链接**:进程启动即需 av*.dll/sw*.dll,新机器需把 `third_party/ffmpeg/bin/` 下 7 个 DLL 拷到 target/debug(deps 同)与 src-tauri/resources/(bundle.resources 已含);DLL 缺失时进程无法启动,无降级。已迁移:probe_video_info(优先 libav,失败回退 CLI 解析);导出/抽帧/录屏仍走 exe,迁移继续分阶段进行。

## 仓库结构与模块划分

```
crates/core      veltrix-core   — 共享库,被桌面端和 server 同时复用,不依赖 Tauri:
                                  config(平台/数据库配置)、db(SeaORM 实体 + 建表)、
                                  api(Axum HTTP /api/v1 + JWT + WS hub + 配对)、error
crates/server    veltrix-server — 可独立部署的 HTTP API 服务二进制,复用 core;
                                  部署形态由 VELTRIX_MODE 决定(cloud / desktop)
src-tauri        veltrix-crawler— 桌面端(bin + lib):
  adapter/       平台解析器(douyin/xhs/kuaishou/bilibili/tiktok/youtube)
  webview/       WebView 池(pool)、原生网络拦截(native_intercept)、脚本注入、cookie、
                 cdp(WebView2 DevTools 协议:DOM.setFileInputFiles 文件上传,发布用)、
                 script_eval(ExecuteScript 同步回读,eval_json_window 供池化窗口用)
  commands/      Tauri 命令(task / collect / dashboard / admin / billing / cloud / creation)
  agent/         桌面操作 Agent(chat / coding / computer / rpa / shell / ocr / uia / orchestrator …)
  cookie/        采集账号池;publish/ 发布服务(独立分类账号池;自动发布流程建设中)
  media/         素材下载;model/ 跨平台统一模型
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

数据库实体在 `crates/core/src/db/entity/`(account、content、comment、task、collect_record、chat_*、customer、publish_account 等 24 张表)。

## 采集数据流(核心,改采集前先读懂)

1. `commands::run_task` 选该平台一个可用账号,后台 `spawn` 异步采集,命令立即返回。
2. `webview::pool` 复用该账号的 WebView 窗口(**per-account 数据目录隔离** = 多账号互不串登录态),导航到搜索页,注入脚本 hook fetch/XHR。
3. 命中平台 `intercept_patterns` 的响应被拦截回传;`run_legacy_scroll` 边滚动边交给 adapter 解析、按去重 `content_id` 计数——**智能停止**:达目标数 / 连续到底 / 网络无响应 / 手动停 即结束。计数排除库中已有 content_id;**去重跳过**:本任务已采 ∪ 去重台账 `collect_records`(同平台、近 90 天)的内容整体跳过,删单条内容不清台账,「清空业务数据」连带清台账。
4. adapter(`DouyinAdapter` / `XhsAdapter` 等,注册在 `lib.rs`)把响应解析为统一 `Content` / `Comment`,**只解析、不发请求**。
5. 边采边入库(on-conflict upsert)。阶段顺序:内容采集 → 作者画像补采 → 评论采集 → 直链补取(开「音频提取」时;刻意排在评论后)→ 素材下载(并发 10 路;**采集窗口保活、账号锁延后到下载结束才释放**——每个并发批从存活窗口取一次轮换后的新会话 Cookie,用户关窗即终止)→ 关窗放锁 → 语音转写 → 封面文字识别(开「封面 OCR」时;智谱 files/ocr,图源为已落盘封面/图集首图,结果落 `contents.cover_ocr_text`,三态同转写:NULL=未识别/失败、空串=无文字)→ 评论意向分析 → Obsidian 同步 → 落 `completed`。

**新增平台** = 加平台配置 + 实现 `PlatformAdapter` trait + 在 `lib.rs` 注册,不改调度/模型/上报。

桌面启动编排(`lib.rs` setup):加载配置 → 连库建表(阻塞)→ spawn 内嵌 HTTP API(`127.0.0.1:8787`)→ spawn 云端 WS 客户端(有 pc_token 则自动拉起)→ 注册适配器 → 建系统托盘(**关闭主窗口是隐藏到托盘,不退进程**)。

## 关键约定(不易从单文件看出)

- **前后端契约**:`src/lib/api.ts` 的 TS 接口(`TaskView` / `ContentView` 等)必须和 `src-tauri/src/commands/*` 里 `#[derive(Serialize)]` + `serde(rename_all="camelCase")` 的 struct 逐字段对应。改一边要同步另一边,否则字段静默变 undefined。
- **数据库迁移**:只用逻辑外键(字段关联,实体 `Relation` 留空),**禁物理 FK**。加字段 = 改 entity + 在 `crates/core/src/db/mod.rs::init_schema` 追加 `ALTER TABLE ... ADD COLUMN ... DEFAULT`(兼容旧库;新建库走 entity DDL,已存在的库走 ALTER)。
- **数据归属**:业务数据记 `owner`(用户名);用户有 `dataScope`(all/self),`list_*` 命令按 scope 过滤。配置类数据(平台/行业/提示词等)共用,不分归属。
- **桌面鉴权**:桌面端登录**不发 token**,登录态存前端 localStorage + 后端 `AppState.current_user`;JWT 仅用于对外 HTTP API(`/api/v1`)。密码哈希用 argon2。
- **任务状态机**:pending → running → downloading_media → completed(失败/手动停为 failed/cancelled)。**completed 算活跃、留在任务列表**,只有 failed/cancelled 进归档 tab。进度靠后端 `task-progress` 事件实时推送 + 前端 2s 轮询兜底(轮询条件必须含 running 与 downloading_media)。
- **转写三态**:`contents.transcript` NULL=未转写/转写失败(可重试,`transcript_error` 存原因),**空串=已转写但未识别到语音**(空文案标记,前端显「空文案」徽章,不再进「待转写」统计与批量重试),非空=文案。「有文案」口径(`require_transcript` 筛选、导出、Obsidian 同步)仍排除空串。
- **列表瘦身视图**:内容库列表(`list_contents_page`)返回 `ContentListView`——不携带 transcript / cover_ocr_text / image_urls / image_paths 等大字段,改由 SQL 端派生 `transcriptState` / `coverOcrState`(`"none"|"empty"|"has"`,与转写三态一一对应)、`transcriptPreview` / `coverOcrOcrPreview`(约 100 字摘要)、`firstImageUrl` / `imageCount`;需要全文的场景(详情、导出 Excel、对话插入文案)走 `getContentDetail` / `list_contents_full`。新增列表字段时同步 `src/lib/api-types.ts` 的 `ContentListView`。
- **平台配置是抓包起点**:`crates/core/src/config/mod.rs` 的 `builtin_default` 里 `search_url_template` / `intercept_patterns` 只是开箱骨架,真实接口路径需本机 `bun run tauri dev` 抓包核对后调整(代码注释已标注)。
- **Tauri 命令注册**:每个新 `#[tauri::command]` 都要加进 `lib.rs` 的 `invoke_handler![]` 列表才能被前端 invoke。
- **采集 WebView 的远程权限**:`src-tauri/capabilities/collect-remote.json` 显式授权小红书/抖音/快手域名 invoke(回传拦截响应与 RPA 结果),新增平台域名要同步加这里。
- **发布账号池独立于采集账号池**:发布账号存 `publish_accounts`,按 CRM 客户分组(`category_id` 存 customers.id,客户在创作 > 客户管理维护,发布侧只读、不再单独建分类表;悬空账号在前端「未关联客户」兜底分组可见),不复用 cookie/ 的轮换/acquire。发布窗口 label 为 `veltrix-pub-{platform}-{accountId}`(保留 `veltrix-` 前缀吃 capabilities 通配,数据目录与采集账号隔离);登录检测脚本 account_id 带 `pub:` 前缀,`login_status_report` 按前缀路由到 `publish::PublishAccounts`,状态机 active / invalid / limited / disabled。
- **视频落盘**:任务开 `keep_video`(TaskFormSheet「保留视频」)时媒体阶段把 mp4 落盘到 `{media_root}/{platform}/{date}/video/{content_id}.mp4` 并回写 `contents.video_path`(默认只抽音频不留视频);发布服务复用此素材。
- **素材路径入库口径**:contents 表本地素材路径列(cover_path/avatar_path/audio_path/video_path/image_paths)统一存相对 media_root 的正斜杠相对路径;读端用 `media::resolve_media_path` 还原为绝对路径(兼容存量绝对路径);写端在回写边界用 `media::to_media_rel` 转换(内存中 MediaOutcome 仍是绝对路径);启动时 `migrate_media_paths_to_relative` 幂等迁移旧数据;前端统一经本地文件服务(端口 8788,/files 前缀,`mediaFileUrl`)访问;本机渲染前缀走 `get_local_file_server_prefix`(恒 `127.0.0.1:8788`,不识别网卡),LAN 前缀 `get_file_server_prefix` 仅留给设置页内网分享展示。
- **素材缩略图**:与源文件同目录,命名 = 源文件名去扩展名 + `_thumb.jpg`(宽 480px 等比、JPEG q80,`thumbnail.rs` 统一实现,纯 Rust image crate 不走 ffmpeg);封面 / 图集 / 头像落盘成功即同步生成(头像换新时旧 thumb 一并作废),文件服务对不存在的 `_thumb.jpg` 请求惰性现生成(生成失败或源图 >20MB 回源),启动时 `thumbnail::backfill_missing` 后台限流回填存量(4 路并发);前端列表 / 卡片 / 作者库头像只加载缩略图,作者库头像本地路径由 AuthorView.avatarPath 按落盘约定({platform}/avatar/{uid}.jpg)探测回填,缺失回退 CDN。文件服务(8788)带协商缓存:弱 ETag `W/"{len:x}-{mtime_secs:x}"` + Last-Modified,命中回 304。

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
