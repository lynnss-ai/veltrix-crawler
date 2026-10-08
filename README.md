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

抖音评论直采遇到可见安全验证时暂停等待人工处理，最多 180 秒；超过时限便结束本批并保留已采评论。接口明确返回拦截页时，直采至多刷新页面令牌并重试一次。超时、停滞等原因不明确时，若配置了 `TYPESAFE_API_KEY`，Jev 只接收错误类别和已收到的响应页数，判断是否值得导航同一视频详情页再试一次；低置信度或模型不可用时直接结束本批。验证码始终由用户在采集窗口手动完成。

小红书搜索固定选择器失效、且未收到有效笔记结果时，Jev 可从页面当前可见的搜索框和提交控件中各选一个，桌面端只重试一次；仍无有效 `/search/notes` 响应即结束本轮，避免采入错误页面。小红书每次应用排序、时间或额外筛选后都等待新的有效笔记响应；未确认筛选生效则停止该轮。评论直采遇到不明确的超时或停滞时，Jev 可判断是否值得打开同一笔记重试一次；批量直采失败会逐条打开笔记回退页面采集。Jev 不接收搜索关键词、笔记内容、Cookie 或接口响应体。

### 低配电脑优化（自动降档）

应用启动时按硬件自动判定低配设备（逻辑 CPU ≤ 4 或物理内存 ≤ 8GB），命中后自动降档以缓解卡顿：采集窗口 HUD 去 backdrop 模糊与常驻动画、暂停平台页自动播放视频，采集并发 3→2、素材下载并发 10→4、缩略图回填 4→2。功能不变，只降并发与视觉成本。判定结果可人工覆盖：启动前设置环境变量 `VELTRIX_LOW_SPEC=1` 强制按低配降档，`=0` 强制按高配跑满。

### 轻载模式（任务级开关，默认开启）

采集窗口在请求层直接拦截页面的视频、字体子资源（返回空响应，不下载不解码），**图片正常显示**（人工盯页面、过滑块验证码不受影响），只拦视频这个最大的解码/流量开销。页面加载更快，低配电脑明显更流畅。封面 / 音频 / 视频素材由 Rust 独立 HTTP 客户端直接从 CDN 下载，不经页面网络栈，完全不受影响；接口拦截与解析链路也不受影响。创建任务表单中可按任务关闭（需要页面完整播放视频时）。

### 仅启动前端（浏览器调试）

```bash
bun run dev
```

### 打包构建

```bash
bun run tauri build
```

构建产物（安装包 / 可执行文件）位于 `src-tauri/target/release/`。
