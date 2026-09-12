# 数据库结构文档

**版本:v2(2026-09-08 更新,对应实体 24 个)**

本文档描述 veltrix-crawler 的数据库表结构,数据源为 `crates/core/src/db/entity/` 下的 24 个 SeaORM 实体文件与 `crates/core/src/db/mod.rs` 的 `init_schema` 函数。相对 v1(`docs/database-schema.md`,23 个实体口径)的主要变化:新增 `publish_accounts` 表(发布账号池);`contents` 追加 `image_paths` / `cover_ocr_text` / `cover_ocr_error` / `video_path` 列;`tasks` 追加 `keep_video` / `cover_ocr` 列;文末补充两张遗留表说明。

## 总览

- **实体与表数量**:`crates/core/src/db/entity/` 下共 **24 个实体**,对应 24 张业务表,全部由 `init_schema` 建表(若不存在)。用户真实库中可能另有 2 张遗留表(`publish_account_categories` / `update_history`),实体代码已移除、仅旧库存在,见文末「遗留表」一节——旧库全库合计 26 张表。
- **双后端**:数据库后端由连接串决定——`sqlite://...` 走本地 SQLite(默认,数据目录下的 `veltrix.db`),`postgres://...` / `postgresql://...` 走 PostgreSQL。连接串优先级:环境变量 `VELTRIX_DATABASE_URL` > 配置文件 > 默认本地 SQLite。同一套 SeaORM 实体跨两种后端复用,建表 DDL 由 `Schema::create_table_from_entity` 按后端方言生成。
- **逻辑外键**:表间关联全部靠字段值关联(实体 `Relation` 均为空枚举),**不建物理外键(FK)**,关联完整性由应用层保证。
- **数据归属**:业务表带 `owner` 字段(记录归属用户名);用户实体有 `data_scope` 字段(`all` / `self`),`list_*` 命令按 scope 过滤数据可见范围。配置类表(行业、提示词、厂商、密钥等)不分归属、全员共用。
- **类型约定**:实体刻意只用基础标量类型,保证两种后端 DDL 通用。`String` → `TEXT`(SeaORM `String` 默认映射为 `VARCHAR`,SQLite 下实为 TEXT 亲和;`column_type = "Text"` 的显式为 TEXT);`i64` → `BIGINT`(SQLite 为 `INTEGER`);`i32` → `INTEGER`;`bool` → `BOOLEAN`(SQLite 存 0/1 整数)。**所有时间字段均为 Unix 秒时间戳(整数),不使用 TIMESTAMP 类型**。JSON 复合字段一律序列化为字符串存 TEXT。
- **迁移方式**:新建库走实体 DDL;已存在的旧库由 `init_schema` 通过 `ALTER TABLE ... ADD COLUMN ... DEFAULT` 追加新列(下方各表中标注「迁移追加」),索引用 `CREATE INDEX IF NOT EXISTS`(标注「迁移索引」),均可幂等重跑。布尔列的迁移默认值一律用 `FALSE` 字面量而非 `0`(PG 布尔列不接受整数默认值,SQLite ≥3.23 两种写法都认)。
- **SQLite 连接参数**:开启 `journal_mode=WAL`、`busy_timeout=5000`、`synchronous=NORMAL`,支撑采集期并发写。

**主键 / 唯一约束一览**:`users.username` 有唯一索引(`idx_users_username`);`collect_records.id`(platform + content_id 拼接)与 `content_synced_users` 的复合主键 `(content_id, synced_user)` 保证天然去重;其余表仅有主键约束。各表主键在字段表中以 **(PK)** 标出。

---

## 一、用户与账号

### users(用户表)

系统用户,密码仅存哈希(argon2),`data_scope` 控制业务数据可见范围。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 用户唯一 ID,业务侧生成,不自增 |
| username | TEXT | 否 | 无 | 登录用户名(唯一索引 `idx_users_username`,迁移索引) |
| password_hash | TEXT | 否 | 无 | 密码哈希(如 argon2 / bcrypt),禁止存明文 |
| email | TEXT | 否 | 无 | 邮箱 |
| nickname | TEXT | 否 | 无 | 昵称 |
| avatar | TEXT | 否 | 无 | 头像 URL 或 base64 data URL,可能较长,用 Text 列 |
| remark | TEXT | 否 | 无 | 备注 |
| status | TEXT | 否 | 无 | 状态:enabled / disabled |
| data_scope | TEXT | 否 | 无 | 数据级别:all(全部数据)/ self(仅自己) |
| obsidian_vault_path | TEXT | 否 | `''` | 该用户的 Obsidian vault 根路径(每用户各自配置);空=未配置,不能同步(迁移追加) |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| updated_at | BIGINT | 否 | 无 | 更新时间(Unix 秒) |
| deleted_at | BIGINT | 否 | 无 | 软删除标记,0 表示未删除 |

逻辑关联:`username` 被各业务表的 `owner` 字段弱关联。

### accounts(账号表)

采集平台账号池(Cookie 管理),字段与领域模型 `cookie::Account` 一一对应。与发布账号池 `publish_accounts` 相互独立:采集账号只管「读」,发布账号只管「写」,风控与频控策略不同,刻意分表避免互相污染。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 账号唯一 ID,业务侧生成,不自增 |
| platform | TEXT | 否 | 无 | 平台 id(douyin / xhs 等) |
| label | TEXT | 否 | 无 | 账号显示名/备注名 |
| cookie | TEXT | 否 | 无 | 完整 Cookie 串,可能较长,用 Text 列 |
| status | TEXT | 否 | 无 | 状态字符串:active / invalid / disabled(历史 cooldown 启动时归并为 active) |
| risk_count | BIGINT | 否 | 无 | 风控触发计数 |
| cooldown_until | BIGINT | 否 | 无 | 历史冷却机制遗留列,已下线,恒为 0 |
| last_used_at | BIGINT | 否 | 无 | 最近使用时间(Unix 秒),「最久未用」轮换依据 |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| code | TEXT | 否 | `''` | 业务编码(如 ACC-XXXX),系统生成(迁移追加) |
| remark | TEXT | 否 | `''` | 备注(迁移追加) |
| owner | TEXT | 否 | `''` | 归属用户(创建者),用于按用户隔离数据(迁移追加) |

索引:`idx_accounts_platform_last_used(platform, last_used_at)`、`idx_accounts_owner(owner)`(迁移索引)。

逻辑关联:`owner` → `users.username`;被 `tasks.account_id` 弱关联。

---

## 二、采集任务

### tasks(采集任务表)

采集任务定义与运行状态,字段映射前端 CollectPage 的 TaskItem。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 任务唯一 ID |
| name | TEXT | 否 | 无 | 任务名称 |
| industry | TEXT | 否 | 无 | 行业名称(冗余存名称而非 industry_id,前端直接读 industries 自由扩展) |
| platform | TEXT | 否 | 无 | 平台 id(platforms.id 的弱关联,逻辑外键) |
| account_id | TEXT | 是 | NULL | 指定采集账号(accounts.id 的弱关联,逻辑外键);None = 按「最久未用」自动轮换(迁移追加) |
| keywords | TEXT | 否 | 无 | 关键词 JSON 数组,例如 `["a","b"]`;定向采集任务存占位词「定向采集」 |
| trigger_type | TEXT | 否 | 无 | 触发类型:once-now / daily / watching |
| scheduled_at | TEXT | 是 | 无 | 每日定时执行时分,格式 HH:mm(仅 trigger_type=daily) |
| watch_interval_min | INTEGER | 是 | 无 | 持续监听轮询分钟数(仅 trigger_type=watching) |
| sort_mode | TEXT | 否 | 无 | 排序方式:synthetic / hottest / latest |
| time_range | TEXT | 否 | 无 | 发布时间范围:any / 1d / 1w / 6m |
| per_keyword_limit | INTEGER | 否 | 无 | 每个关键词最多返回条数 |
| min_likes | INTEGER | 否 | 无 | 最低点赞数(<该值丢弃) |
| audio_extract | BOOLEAN | 否 | 0 | 是否启用音频提取(视频下载并转 mp3 留存;AI 文案提取开启时隐含开启)(迁移追加;追加时按 `ai_extract=1` 回填一次) |
| keep_video | BOOLEAN | 否 | 0 | 是否保留视频文件:采集后视频落盘不清理(路径回写 `contents.video_path`),供发布服务复用素材;默认关,发布后源文件由发布侧管理(迁移追加) |
| ai_extract | BOOLEAN | 否 | 无 | 是否启用 AI 文案提取(依赖音频提取:转音频后做语音转写) |
| cover_ocr | BOOLEAN | 否 | 0 | 是否启用封面文字识别(采集后对封面图做 OCR,智谱;结果存 `contents.cover_ocr_text`)(迁移追加) |
| collect_comments | BOOLEAN | 否 | 0 | 是否采集评论(开启后内容采集完进入评论采集阶段)(迁移追加) |
| comment_time_range | TEXT | 否 | `'any'` | 评论发布时间范围过滤:3d / 7d / 14d / any(不限)(迁移追加) |
| comment_limit | INTEGER | 否 | 0 | 单视频一级评论采集上限,0 表示不限(迁移追加) |
| analyze_comment_intent | BOOLEAN | 否 | 0 | 是否对评论做 AI 意图分析(迁移追加) |
| status | TEXT | 否 | 无 | 运行状态:pending / running / collecting_comments / downloading_media / completed / failed / cancelled |
| progress | INTEGER | 否 | 无 | 进度 0-100 |
| media_total | INTEGER | 否 | 0 | 素材下载总数(进入 downloading_media 时确定 = 去重后待下载内容数,0 表示无素材)(迁移追加) |
| media_done | INTEGER | 否 | 0 | 素材已处理数(成功 + 失败均计入),= media_total 时任务转 completed(迁移追加) |
| comment_video_total | INTEGER | 否 | 0 | 评论采集阶段:待采视频总数(进入 collecting_comments 时确定 = 去重后内容数)(迁移追加) |
| comment_video_done | INTEGER | 否 | 0 | 评论采集阶段:已采视频数,= comment_video_total 时转入素材下载 / 完成(迁移追加) |
| content_count | BIGINT | 否 | 无 | 已采集内容数 |
| comment_count | BIGINT | 否 | 无 | 已采集评论数 |
| started_at | BIGINT | 是 | 无 | 首次启动时间(Unix 秒) |
| finished_at | BIGINT | 是 | 无 | 结束时间(Unix 秒,归档后填) |
| error_message | TEXT | 是 | 无 | 失败原因(仅 status=failed) |
| owner | TEXT | 否 | 无 | 数据归属:任务所属用户名(users.username 的弱关联) |
| archived | BOOLEAN | 否 | 0 | 是否已归档(手动归档后移入归档 tab;终止/失败不自动归档)(迁移追加) |
| auto_sync_obsidian | BOOLEAN | 否 | 0 | 采集完成后是否自动同步内容到发起者(owner)的 Obsidian vault(迁移追加) |
| extra_filters | TEXT | 否 | `'{}'` | 平台专属额外筛选维度(如抖音:视频时长 / 搜索范围 / 内容形式),JSON 对象 `{维度id: 选中文案}`,空对象 = 全「不限」;采集时在结果页「筛选」浮层按选中文案点击应用(迁移追加) |
| target_urls | TEXT | 否 | `'[]'` | 定向采集目标链接 JSON 数组(视频链接 / 作者主页链接);空数组 = 关键词搜索任务(迁移追加) |
| max_retries | INTEGER | 否 | 0 | 失败自动重试次数上限(0=不自动重试);失败后按 1min / 5min / 15min 指数退避重跑,只重试「瞬时可恢复」的失败(开窗失败 / 导航无响应 / 网络抖动)(迁移追加) |
| retry_count | INTEGER | 否 | 0 | 当前失败序列已自动重试的次数(成功或手动重跑新序列后归零)(迁移追加) |
| next_retry_at | BIGINT | 是 | NULL | 下次自动重试时间(Unix 秒);None=未排期(未开重试 / 已耗尽 / 运行中 / 成功)(迁移追加) |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| updated_at | BIGINT | 否 | 无 | 更新时间(Unix 秒) |

索引:`idx_tasks_owner_updated(owner, updated_at)`、`idx_tasks_status(status)`(迁移索引)。

逻辑关联:`owner` → `users.username`;`account_id` → `accounts.id`;被 `contents.task_id` / `comments.task_id` / `collect_logs.task_id` / `task_runs.task_id` 关联。

### task_runs(任务执行历史表)

每次运行 `run_task` 记一条:起止时间、终态、本次新增量。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 运行 id(`{task_id}-run-{started_ts}`,同任务两次运行起始秒不同,唯一) |
| task_id | TEXT | 否 | 无 | 所属任务(tasks.id 的逻辑外键) |
| owner | TEXT | 否 | 无 | 数据归属:继承任务 owner |
| started_at | BIGINT | 否 | 无 | 本次运行开始时间(Unix 秒);与该次采集内容的 collected_at 起点一致 |
| finished_at | BIGINT | 是 | 无 | 本次运行结束时间(Unix 秒);运行中为 None |
| status | TEXT | 否 | 无 | 终态:running / completed / failed / cancelled |
| content_delta | BIGINT | 否 | 无 | 本次新增内容数(collected_at >= started_at,即排除重复采到的已有内容) |
| comment_delta | BIGINT | 否 | 无 | 本次新增评论数 |
| error_message | TEXT | 是 | 无 | 失败原因;None 表示无 |
| metrics_json | TEXT | 是 | NULL | 本次运行的采集指标 JSON(拦截响应数 / 解析失败数 / 入库数 / 各阶段耗时等),供事后排查与横向对比;老数据为 None(迁移追加) |

逻辑关联:`task_id` → `tasks.id`;`owner` → `users.username`。「采集日志」按时间范围 `(started_at, finished_at)` 关联 `collect_logs`(同账号采集串行,时间不重叠)。

### collect_logs(采集日志表)

每条 collect-log 事件持久化一行,供任务详情页加载历史日志。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | BIGINT | 否 | 自增 | 自增主键(日志量大,用整型自增而非 UUID) |
| task_id | TEXT | 否 | 无 | 所属任务(tasks.id 的逻辑外键) |
| ts | BIGINT | 否 | 无 | 产生时间(Unix 秒) |
| level | TEXT | 否 | 无 | 级别:info / warn / error |
| message | TEXT | 否 | 无 | 日志文本 |
| entry_json | TEXT | 是 | 无 | 富条目(内容/评论)JSON;普通日志为 None |

索引:`idx_collect_logs_task(task_id, ts)`(迁移索引)。

逻辑关联:`task_id` → `tasks.id`。

### collect_records(采集去重台账表)

每条已采集内容记一行 (platform, content_id),独立于业务数据:删除单条内容不清台账(被删内容不会被重采找回),「清空业务数据」连带清空。再次采集时据此判重:曾采过的内容整体跳过(不再入库,评论 / 素材阶段也不处理)。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 主键 = `ledger_key(platform, content_id)`,即 `{platform}::{content_id}`,保证同平台同内容全局唯一(用 `::` 分隔,避免平台 id 含连字符时与内容 id 边界混淆) |
| platform | TEXT | 否 | 无 | 平台 id(douyin / xhs ...) |
| content_id | TEXT | 否 | 无 | 平台侧内容 id(去重的核心维度) |
| created_at | BIGINT | 否 | 无 | 首次采集(登记)时间,Unix 秒 |

索引:`idx_collect_records_platform(platform, content_id)`(迁移索引)。

逻辑关联:`content_id` 与 `contents.content_id` 同维度(跨任务去重,不指向单条内容行)。

---

## 三、内容与评论

### contents(采集内容表)

平台适配器解析出的统一内容模型落库;与任务绑定,同任务重采前按 task_id 清旧再插新,保证为最近一次采集快照。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 主键:`{task_id}-{platform}-{content_id}`,同任务内对同一内容去重(重采覆盖) |
| task_id | TEXT | 否 | 无 | 所属任务(tasks.id 的逻辑外键) |
| platform | TEXT | 否 | 无 | 平台 id(platforms.id 的弱关联) |
| content_id | TEXT | 否 | 无 | 平台内内容唯一 ID(抖音 aweme_id / 小红书 note_id / 快手 photo_id) |
| keyword | TEXT | 否 | `''` | 采集时命中的关键词(由 run_task 当前 keyword 填),用于全量库按词筛选(迁移追加) |
| kind | TEXT | 否 | 无 | 内容形态:video / image / article / unknown |
| title | TEXT | 是 | 无 | 标题 |
| desc | TEXT | 是 | 无 | 描述/正文摘要 |
| author_uid | TEXT | 否 | 无 | 作者平台内 UID |
| author_nickname | TEXT | 否 | 无 | 作者昵称(采集时刻快照) |
| author_json | TEXT | 否 | 无 | 完整作者信息 JSON(头像/签名/粉丝数等,避免频繁加列) |
| like_count | BIGINT | 是 | 无 | 点赞数 |
| comment_count | BIGINT | 是 | 无 | 评论数 |
| collect_count | BIGINT | 是 | 无 | 收藏数 |
| share_count | BIGINT | 是 | 无 | 分享数 |
| play_count | BIGINT | 是 | 无 | 播放数 |
| published_at | BIGINT | 是 | 无 | 发布时间(Unix 秒) |
| video_url | TEXT | 是 | 无 | 无水印视频直链(视频且解析成功时) |
| cover_url | TEXT | 是 | NULL | 封面图地址:视频封面 / 图文首图(迁移追加) |
| image_urls | TEXT | 否 | 无 | 图片地址 JSON 数组,例如 `["url1","url2"]` |
| image_paths | TEXT | 是 | NULL | 图集本地路径 JSON,按 `image_urls` 下标一一对应;独立保存,封面失败也不影响其他已下载图片的读取;旧数据为 None(迁移追加) |
| duration | BIGINT | 是 | NULL | 视频时长(秒);图文为 None(迁移追加) |
| topics | TEXT | 否 | `'[]'` | 话题标签 JSON 数组(# 开头),例如 `["#话题a","#话题b"]`(迁移追加) |
| extra | TEXT | 否 | 无 | 平台特有字段原始 JSON |
| owner | TEXT | 否 | 无 | 数据归属:继承任务 owner(users.username 弱关联) |
| collected_at | BIGINT | 否 | 无 | 采集时间(Unix 秒) |
| media_status | TEXT | 是 | NULL | 素材下载状态:pending / success / failed;None=旧数据,未跑过下载(迁移追加) |
| audio_extracted | BOOLEAN | 是 | NULL | 音频是否提取成功:仅「视频 + 开启音频提取」时有意义,其余为 None(迁移追加) |
| media_error | TEXT | 是 | NULL | 素材失败原因(视频 403 / ffmpeg 转码失败等),供前端展示与失败重试判断(迁移追加) |
| cover_path | TEXT | 是 | NULL | 封面本地绝对路径(下载成功后回写);前端本地优先显示,None=未下载/失败回退外链(迁移追加) |
| avatar_path | TEXT | 是 | NULL | 作者头像本地绝对路径(下载成功/已存在后回写);None=未下载/失败回退外链(迁移追加) |
| audio_path | TEXT | 是 | NULL | 视频转出音频(mp3 等)本地绝对路径;仅「视频 + 音频提取成功」时有值,详情页播放用(迁移追加) |
| transcript | TEXT | 是 | NULL | 视频语音转写文本;仅视频且转写成功时有值,None=未转写/非视频/失败(迁移追加) |
| transcript_error | TEXT | 是 | NULL | 转写失败原因(供前端区分「未转写」与「转写失败」)(迁移追加) |
| cover_ocr_text | TEXT | 是 | NULL | 封面图 OCR 识别文本(智谱 OCR,任务开「封面文字识别」时)。三态:NULL=未识别或识别失败、空串=已识别但无文字、非空=封面文案(迁移追加) |
| cover_ocr_error | TEXT | 是 | NULL | 封面 OCR 失败原因(供前端区分「未识别」与「识别失败」)(迁移追加) |
| video_downloaded | BOOLEAN | 是 | NULL | 视频文件是否下载成功(仅 video + ai_extract);None=非视频/未尝试(迁移追加) |
| image_total | INTEGER | 是 | NULL | 图文图片总数(仅 image);None=非图文(迁移追加) |
| image_done | INTEGER | 是 | NULL | 图文已成功下载数(仅 image);None=非图文(迁移追加) |
| comment_collected | BOOLEAN | 是 | NULL | 是否已采集评论(评论采集阶段后回写);None=未到评论阶段/未开评论采集(迁移追加) |
| intent_analyzed | BOOLEAN | 是 | NULL | 是否已做意向分析(意向分析后回写);None=未分析(迁移追加) |
| video_path | TEXT | 是 | NULL | 视频文件本地落盘路径(任务开「保留视频」时下载留存,发布服务复用该素材);None=未落盘(迁移追加) |

索引:`idx_contents_task(task_id)`、`idx_contents_task_platform_content(task_id, platform, content_id)`、`idx_contents_owner_collected(owner, collected_at)`、`idx_contents_collected(collected_at)`、`idx_contents_like_count(like_count)`、`idx_contents_platform_content(platform, content_id)`、`idx_contents_owner_platform_author(owner, platform, author_uid)`(迁移索引)。

逻辑关联:`task_id` → `tasks.id`;`owner` → `users.username`;`platform + author_uid` → `authors`;`id` 被 `content_synced_users.content_id` 关联。

### comments(采集评论表)

采集评论落库,与任务绑定;`parent_id` 为空表示一级评论,非空指向其一级评论。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 主键:`{task_id}-{platform}-{comment_id}`,同任务内对同一评论去重 |
| task_id | TEXT | 否 | 无 | 所属任务(tasks.id 的逻辑外键) |
| platform | TEXT | 否 | 无 | 平台 id(platforms.id 的弱关联) |
| content_id | TEXT | 否 | 无 | 所属内容的平台内 ID(contents.content_id 的弱关联) |
| comment_id | TEXT | 否 | 无 | 平台内评论唯一 ID |
| parent_id | TEXT | 是 | 无 | 父评论 ID;一级评论为空,楼中楼回复指向其一级评论 |
| author_uid | TEXT | 否 | 无 | 评论作者平台内 UID |
| author_nickname | TEXT | 否 | 无 | 评论作者昵称 |
| author_json | TEXT | 否 | 无 | 完整作者信息 JSON |
| text | TEXT | 否 | 无 | 评论正文 |
| like_count | BIGINT | 是 | 无 | 点赞数 |
| reply_count | BIGINT | 是 | 无 | 回复数 |
| created_at | BIGINT | 是 | 无 | 评论发表时间(Unix 秒) |
| owner | TEXT | 否 | 无 | 数据归属:继承任务 owner(users.username 弱关联) |
| collected_at | BIGINT | 否 | 无 | 采集时间(Unix 秒) |
| intent_level | TEXT | 是 | NULL | AI 意向分析等级:high / medium / low / none;None=尚未分析(迁移追加) |
| intent_reason | TEXT | 是 | NULL | AI 意向分析理由;None=尚未分析(迁移追加) |

索引:`idx_comments_task(task_id)`、`idx_comments_content(content_id)`、`idx_comments_owner_collected(owner, collected_at)`、`idx_comments_collected(collected_at)`、`idx_comments_intent(intent_level)`(迁移索引)。

逻辑关联:`task_id` → `tasks.id`;`task_id + platform + content_id` → `contents`(评论归因 JOIN);`owner` → `users.username`。

### authors(作者表)

平台创作者去重档案,按 (owner, platform, uid) upsert 刷新最新画像。content/comment 仍各存作者快照(采集那一刻),本表提供「一个作者一行、可更新、可监控」的聚合视角。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 主键:`{owner}-{platform}-{uid}`,兼顾数据归属与作者去重 |
| owner | TEXT | 否 | 无 | 数据归属(users.username 弱关联) |
| platform | TEXT | 否 | 无 | 平台 id |
| uid | TEXT | 否 | 无 | 作者 UID(抖音为 sec_uid) |
| nickname | TEXT | 否 | 无 | 昵称 |
| avatar | TEXT | 是 | 无 | 头像地址 |
| platform_id | TEXT | 是 | 无 | 平台号(抖音号 unique_id 等) |
| short_id | TEXT | 是 | 无 | 平台短 ID(extra.uid) |
| signature | TEXT | 是 | 无 | 签名/简介 |
| follower_count | BIGINT | 是 | 无 | 粉丝数 |
| following_count | BIGINT | 是 | 无 | 关注数 |
| total_favorited | BIGINT | 是 | 无 | 作者获赞总数(部分平台返回,缺失为 None) |
| location | TEXT | 是 | 无 | IP 属地(部分平台返回,缺失为 None) |
| is_monitored | BOOLEAN | 否 | 无 | 是否被持续监控(作者级监控开关) |
| is_blacklisted | BOOLEAN | 否 | 0 | 是否被拉黑(作者级黑名单开关):采集时命中该作者的内容会被排除、不抓(迁移追加) |
| first_collected_at | BIGINT | 否 | 无 | 首次采集时间(Unix 秒) |
| last_collected_at | BIGINT | 否 | 无 | 最近采集时间(Unix 秒) |

索引:`idx_authors_owner(owner)`、`idx_authors_platform_uid(platform, uid)`、`idx_authors_monitored(is_monitored)`、`idx_authors_blacklisted(is_blacklisted)`(迁移索引)。

逻辑关联:`owner` → `users.username`;`platform + uid` 与 `contents.platform + author_uid` 对应。

### content_synced_users(内容-用户同步追踪表)

记录「某用户已把某条内容同步到其 Obsidian vault」,复合主键幂等:同一用户对同一内容只记最新一次同步。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| content_id **(PK)** | TEXT | 否 | 无 | 内容主键(contents.id,复合 `{task_id}-{platform}-{content_id}`);与 synced_user 组成复合主键 |
| synced_user **(PK)** | TEXT | 否 | 无 | 同步该内容的用户名(users.username 弱关联) |
| synced_at | BIGINT | 否 | 无 | 最近一次同步时间(Unix 秒) |
| vault_path | TEXT | 否 | 无 | 同步目标 vault 根路径(便于排查/未来多 vault) |

索引:`idx_content_synced_users_user(synced_user)`、`idx_content_synced_users_content(content_id)`(迁移索引)。

逻辑关联:`content_id` → `contents.id`;`synced_user` → `users.username`。

---

## 四、客户与行业

### customers(客户表)

客户管理 / CRM,tags 以 JSON 字符串存多标签。发布账号(`publish_accounts.category_id`)按本表客户分组,不再单独维护发布分类表。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 客户唯一 ID |
| code | TEXT | 否 | 无 | 客户编号(如 CUS-XXXX),系统生成 |
| name | TEXT | 否 | 无 | 客户姓名 |
| phone | TEXT | 否 | 无 | 电话 |
| email | TEXT | 否 | 无 | 邮箱 |
| company | TEXT | 否 | 无 | 公司 |
| position | TEXT | 否 | 无 | 职位 |
| wechat | TEXT | 否 | 无 | 微信号 |
| industry | TEXT | 否 | 无 | 所属行业(名称或行业 code) |
| tags | TEXT | 否 | 无 | 标签数组,以 JSON 字符串存储(如 ["高意向","KOL"]) |
| source | TEXT | 否 | 无 | 客户来源 |
| status | TEXT | 否 | 无 | 客户状态:new / following / negotiating / closed / lost / dormant |
| owner | TEXT | 否 | 无 | 归属用户(跟踪人) |
| remark | TEXT | 否 | 无 | 备注 |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| updated_at | BIGINT | 否 | 无 | 更新时间(Unix 秒) |

索引:`idx_customers_owner(owner)`(迁移索引)。

逻辑关联:`owner` → `users.username`;`industry` 与 `industries`(名称或 code)弱关联;被 `publish_accounts.category_id` 关联。

### industries(行业表)

行业类别,用于归类采集关键词;配置类数据,不分归属。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 行业唯一 ID |
| code | TEXT | 否 | 无 | 业务编码(如 IND-XXXX),系统生成 |
| name | TEXT | 否 | 无 | 行业名称 |
| remark | TEXT | 否 | 无 | 备注 |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| updated_at | BIGINT | 否 | 无 | 更新时间(Unix 秒) |

逻辑关联:被 `keywords.industry_id` 关联;`tasks.industry` 冗余存其名称。

### keywords(关键词表)

行业类别下的关键词,用于驱动采集。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 关键词唯一 ID |
| industry_id | TEXT | 否 | 无 | 所属行业 ID(industries.id) |
| word | TEXT | 否 | 无 | 关键词文本 |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |

索引:`idx_keywords_industry(industry_id)`(迁移索引)。

逻辑关联:`industry_id` → `industries.id`。

---

## 五、AI 对话与用量

### chat_conversations(AI 对话会话表)

对话工作区,每个会话绑定一个模型厂商 + 模型,归属用户。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 会话 id(前端生成 UUID) |
| owner | TEXT | 否 | 无 | 数据归属:创建者用户名 |
| title | TEXT | 否 | 无 | 会话标题(首条消息后自动生成,可手动改) |
| provider_id | TEXT | 否 | 无 | 所用模型厂商 id(providers.id 逻辑引用) |
| model | TEXT | 否 | 无 | 所用模型名 |
| agent_type | TEXT | 否 | `'chat'` | 场景类型:chat / coding / rpa …(默认 chat)。决定走哪个 Agent 与页面布局(迁移追加) |
| summary | TEXT | 否 | `''` | 滚动摘要:本会话早期(已滚出 live 窗口)消息压缩后的「前情提要」,发送时作 system 注入(迁移追加) |
| summarized_upto_id | BIGINT | 否 | 0 | 已折叠进 `summary` 的最大消息 id;id 大于此值的消息为 live 原文(不重复进摘要)(迁移追加) |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| updated_at | BIGINT | 否 | 无 | 更新时间(Unix 秒) |
| archived | BOOLEAN | 否 | 0 | 是否归档:归档会话从「最近对话」与对话页隐藏,仅在对话记录页可见 / 可恢复(迁移追加) |
| plan_todos | TEXT | 否 | `''` | 编程 Agent 的结构化任务清单(JSON 数组 `[{"title","done"}]`):Plan 模式调研后产出、Act 模式按序执行并勾选;空串表示尚无计划(迁移追加) |

索引:`idx_chat_conversations_owner(owner, updated_at)`(迁移索引)。

逻辑关联:`owner` → `users.username`;`provider_id` → `providers.id`;被 `chat_messages.conversation_id` 关联。

### chat_messages(AI 对话消息表)

每条消息属于一个会话。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | BIGINT | 否 | 自增 | 自增主键(消息量大,用整型自增) |
| conversation_id | TEXT | 否 | 无 | 所属会话(chat_conversations.id) |
| role | TEXT | 否 | 无 | 角色:user / assistant(工具返回为 tool) |
| content | TEXT | 否 | 无 | 消息正文 |
| tool_calls | TEXT | 是 | NULL | assistant 要求调用的工具(JSON 数组 [{id,name,arguments}]);纯文本回复为 None(迁移追加) |
| tool_call_id | TEXT | 是 | NULL | role=tool 时:对应的工具调用 id(关联上一条 assistant 的某次 tool_call)(迁移追加) |
| tool_name | TEXT | 是 | NULL | role=tool 时:工具名(便于前端展示)(迁移追加) |
| attachments | TEXT | 是 | NULL | user 消息携带的附件元数据(JSON 数组 `[{name,mime,path}]`;图片 path 为本地绝对路径,供历史渲染与多轮多模态重建;非图片附件 path 为空,仅作文件名展示)(迁移追加) |
| reasoning | TEXT | 是 | NULL | assistant 的思考过程(Claude thinking 块 / DeepSeek reasoning_content),仅推理型模型非空;供前端「思考过程」折叠块展示(迁移追加) |
| feedback | TEXT | 是 | NULL | 用户反馈:like / dislike / null(未反馈),用于学习与适应功能(迁移追加) |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |

索引:`idx_chat_messages_conversation(conversation_id, id)`(迁移索引)。

逻辑关联:`conversation_id` → `chat_conversations.id`。

### chat_memories(AI 对话长期记忆表)

跨会话、按用户归属的记忆条目,发消息前把启用的记忆拼成 system 消息注入上下文。来源:`auto`(LLM 每轮自动从对话中提取)/ `manual`(用户在设置页手动维护)。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | BIGINT | 否 | 自增 | 自增主键(记忆条数可能较多,用整型自增) |
| owner | TEXT | 否 | 无 | 数据归属:用户名 |
| scope | TEXT | 否 | `'global'` | 记忆作用域:global(全局)/ project(项目)/ conversation(会话)(迁移追加) |
| scope_id | TEXT | 否 | `''` | 作用域 ID:project 时为项目 ID,conversation 时为会话 ID,global 时为空(迁移追加) |
| content | TEXT | 否 | 无 | 记忆内容(一条自包含的事实 / 偏好) |
| source | TEXT | 否 | 无 | 来源:`auto`(自动提取)/ `manual`(手动添加) |
| enabled | BOOLEAN | 否 | 无 | 是否启用:关闭后不注入上下文,但保留可恢复 |
| embedding | TEXT | 是 | NULL | 内容向量(JSON float 数组字符串);None=尚未生成,按当前问题语义检索 top-K 注入(RAG)用(迁移追加) |
| embed_model | TEXT | 是 | NULL | 生成该向量所用的 embedding 模型;换模型后据此判定旧向量失效、需重算(迁移追加) |
| pinned | BOOLEAN | 否 | 0 | 置顶:每轮对话恒注入,不参与相似度淘汰(给「称呼/职业」等永远要带的事实)(迁移追加) |
| mem_type | TEXT | 否 | `'other'` | 分类:identity(身份)/ preference(偏好)/ project(项目)/ relationship(人际)/ habit(习惯)/ other(其它)(迁移追加) |
| importance | INTEGER | 否 | 3 | 重要度 1-5:越高越优先注入、淘汰时越靠后(迁移追加) |
| confidence | INTEGER | 否 | 3 | 置信度 1-5:模型对该记忆的确定程度;低置信优先被淘汰(迁移追加) |
| hit_count | BIGINT | 否 | 0 | 命中次数:每次被注入 +1,衡量记忆实际有用程度(迁移追加) |
| last_hit_at | BIGINT | 否 | 0 | 最后命中时间(Unix 秒):时间衰减用,久未命中的逐渐降权(迁移追加) |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| updated_at | BIGINT | 否 | 无 | 更新时间(Unix 秒) |

索引:`idx_chat_memories_owner(owner, updated_at)`(迁移索引)。

逻辑关联:`owner` → `users.username`;`scope_id`(scope=conversation 时)→ `chat_conversations.id`。

### model_usage_records(模型用量记录表)

每次 LLM 调用产生一条记录,用于账单统计与用量分析。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | BIGINT | 否 | 自增 | 自增主键 |
| model | TEXT | 否 | 无 | 模型名(deepseek-chat, qwen-plus 等) |
| provider_id | TEXT | 否 | 无 | 厂商 ID(providers.id) |
| prompt_tokens | BIGINT | 否 | 无 | 输入 token |
| completion_tokens | BIGINT | 否 | 无 | 输出 token |
| total_tokens | BIGINT | 否 | 无 | 合计 token |
| source | TEXT | 否 | 无 | 来源:chat / agent_chat / coding / rpa / computer / transcription(语音转写) |
| owner | TEXT | 否 | 无 | 归属用户 |
| created_at | BIGINT | 否 | 无 | Unix 秒时间戳 |

索引:`idx_model_usage_created(created_at)`、`idx_model_usage_owner(owner, created_at)`、`idx_model_usage_model(model, created_at)`(迁移索引)。

逻辑关联:`owner` → `users.username`;`provider_id` → `providers.id`。

### agent_route_logs(Agent 路由遥测表)

每条新会话首条消息的意图路由决策记一条,用于长期分析「哪些输入老路由错」,作为调关键词 / description / 上分层的依据——没有这层数据,路由优化全靠拍脑袋。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | BIGINT | 否 | 自增 | 自增主键 |
| text | TEXT | 否 | 无 | 用户首条消息(截断至前 500 字,仅留前若干字供人工核对,防长文案塞爆遥测表) |
| keyword_route | TEXT | 否 | 无 | 关键词启发式得到的路由(chat/coding/rpa/computer/local) |
| llm_used | BOOLEAN | 否 | 无 | 是否触发了 LLM 兜底分类(仅关键词落到 chat 且像可执行任务时才触发) |
| llm_route | TEXT | 否 | 无 | LLM 兜底给出的路由(未触发为空串) |
| final_route | TEXT | 否 | 无 | 最终返回的路由 |
| model | TEXT | 否 | 无 | LLM 兜底所用模型(未触发为空串) |
| owner | TEXT | 否 | 无 | 归属用户 |
| created_at | BIGINT | 否 | 无 | Unix 秒时间戳 |

索引:`idx_agent_route_created(created_at)`、`idx_agent_route_owner(owner, created_at)`(迁移索引)。

逻辑关联:`owner` → `users.username`。

---

## 六、提示词与厂商配置

### prompts(提示词表)

系统配置 - 提示词;配置类数据,不分归属。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 提示词唯一 ID |
| code | TEXT | 否 | 无 | 业务编码(如 PRM-XXXX),系统生成 |
| name | TEXT | 否 | 无 | 提示词名称 |
| content | TEXT | 否 | 无 | 提示词正文 |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| updated_at | BIGINT | 否 | 无 | 更新时间(Unix 秒) |

### prompt_categories(提示词分类目录表)

内容创作 - 提示词管理:用户自定义分类目录(如 图像分镜 / 视频分镜 / 镜头景别),下挂多条分镜镜头提示词。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 分类目录唯一 ID |
| owner | TEXT | 否 | 无 | 归属用户 |
| name | TEXT | 否 | 无 | 分类名称 |
| remark | TEXT | 否 | 无 | 备注 |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| updated_at | BIGINT | 否 | 无 | 更新时间(Unix 秒) |

索引:`idx_prompt_categories_owner(owner)`(迁移索引)。

逻辑关联:`owner` → `users.username`;被 `shot_prompts.category_id` 关联。

### shot_prompts(分镜镜头提示词表)

每条提示词归属一个分类目录。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 提示词唯一 ID |
| owner | TEXT | 否 | 无 | 归属用户 |
| category_id | TEXT | 否 | 无 | 所属分类目录 ID(prompt_categories.id) |
| name | TEXT | 否 | 无 | 提示词标题 |
| content | TEXT | 否 | 无 | 提示词正文 |
| remark | TEXT | 否 | 无 | 备注 |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| updated_at | BIGINT | 否 | 无 | 更新时间(Unix 秒) |

索引:`idx_shot_prompts_owner(owner)`、`idx_shot_prompts_category(category_id)`(迁移索引)。

逻辑关联:`owner` → `users.username`;`category_id` → `prompt_categories.id`。

### providers(模型厂商表)

系统配置 - 模型厂商(OpenAI 兼容厂商);配置类数据,不分归属。models 列的解析/序列化见 `src-tauri` 的 `llm::provider`。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 厂商唯一 ID |
| code | TEXT | 否 | 无 | 业务编码(如 PRV-XXXX),系统生成 |
| name | TEXT | 否 | 无 | 厂商名称 |
| api_url | TEXT | 否 | 无 | API 基础地址 |
| api_key | TEXT | 否 | 无 | API 密钥 |
| models | TEXT | 否 | 无 | 可用模型:结构化列表 JSON(`[{"name","capabilities":[...]}]`,名称 + 能力);兼容旧多行文本(每行一个模型名,降级为仅对话能力) |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |
| updated_at | BIGINT | 否 | 无 | 更新时间(Unix 秒) |

逻辑关联:被 `chat_conversations.provider_id`、`model_usage_records.provider_id` 关联。

### app_secrets(密钥键值表)

存 api_key 等敏感配置(api_key 不落配置文件,统一存数据库);配置类数据,不分归属。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| key **(PK)** | TEXT | 否 | 无 | 用途标识(如 intent_api_key / transcription_api_key) |
| value | TEXT | 否 | 无 | 明文密钥 |

---

## 七、发布服务

### publish_accounts(发布账号表)

发布服务的账号池,与采集账号池(`accounts`)相互独立:采集账号只管「读」,发布账号只管「写」,风控与频控策略不同,刻意分表避免互相污染。发布账号按 CRM 客户分组(`category_id` 存 `customers.id`,客户在运营 > 客户管理维护),不再单独维护分类表(旧库的 `publish_account_categories` 遗留表即被此设计替代)。Cookie 按账号隔离使用,禁止打印日志。

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 账号唯一 ID,业务侧生成,不自增 |
| platform | TEXT | 否 | 无 | 平台 id(douyin / xhs 等,platforms.id 的弱关联) |
| category_id | TEXT | 否 | 无 | 所属客户(customers.id 的逻辑外键):发布账号按 CRM 客户分组 |
| label | TEXT | 否 | 无 | 备注名(用户起的展示名,与平台昵称解耦,昵称变了不影响识别) |
| nickname | TEXT | 否 | `''` | 平台侧昵称,导入后可能未回填 |
| avatar | TEXT | 否 | `''` | 平台侧头像地址 |
| uid | TEXT | 否 | `''` | 平台侧 uid(发布接口需要;导入 Cookie 时未必能解析出) |
| cookie | TEXT | 否 | 无 | 完整 Cookie 串,可能较长,用 Text 列;按账号隔离使用,禁止打印日志 |
| status | TEXT | 否 | 无 | 状态字符串:active / invalid(失效)/ limited(被限流)/ disabled(手动停用) |
| fail_reason | TEXT | 否 | 无 | 最近一次失败 / 受限原因(风控提示、接口报错等),供前端展示与人工处置;Text 列 |
| code | TEXT | 否 | 无 | 业务编码(如 PAC-XXXX),系统生成,对齐 accounts 表的 code |
| last_login_at | BIGINT | 否 | 无 | 最近一次登录 / Cookie 校验成功时间(Unix 秒),用于判断登录态新鲜度 |
| last_publish_at | BIGINT | 否 | 无 | 最近一次发布成功时间(Unix 秒),0 = 从未发布 |
| today_published | BIGINT | 否 | 无 | 当日已发布条数,频控用;与 today_date 配合 |
| today_date | TEXT | 否 | 无 | 当日日期(YYYY-MM-DD):发布时若与当前日期不一致,先把 today_published 清零再计数,用「字段比对」替代定时任务实现跨天滚动清零 |
| owner | TEXT | 否 | 无 | 归属用户(创建者),用于按用户隔离数据 |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |

索引:无(`init_schema` 未为本表建二级索引)。

逻辑关联:`owner` → `users.username`;`category_id` → `customers.id`;`platform` → `platforms.id`(弱关联)。

---

## 逻辑关联关系汇总

所有关联均为逻辑外键(字段值关联,无物理 FK):

- `accounts.owner` → `users.username`
- `tasks.owner` → `users.username`;`tasks.account_id` → `accounts.id`;`tasks.industry` → `industries.name`(冗余名称)
- `task_runs.task_id` → `tasks.id`;`task_runs.owner` → `users.username`;`(started_at, finished_at)` 时间范围关联 `collect_logs`
- `collect_logs.task_id` → `tasks.id`
- `collect_records.content_id` 与 `contents.content_id` 同维度(跨任务去重)
- `contents.task_id` → `tasks.id`;`contents.owner` → `users.username`;`(platform, author_uid)` → `authors.(platform, uid)`
- `comments.task_id` → `tasks.id`;`comments.(task_id, platform, content_id)` → `contents`;`comments.owner` → `users.username`
- `authors.owner` → `users.username`
- `content_synced_users.content_id` → `contents.id`;`content_synced_users.synced_user` → `users.username`
- `customers.owner` → `users.username`;`customers.industry` → `industries`(名称或 code)
- `keywords.industry_id` → `industries.id`
- `chat_conversations.owner` → `users.username`;`chat_conversations.provider_id` → `providers.id`
- `chat_messages.conversation_id` → `chat_conversations.id`
- `chat_memories.owner` → `users.username`;`scope=conversation` 时 `scope_id` → `chat_conversations.id`
- `model_usage_records.owner` → `users.username`;`model_usage_records.provider_id` → `providers.id`
- `agent_route_logs.owner` → `users.username`
- `prompt_categories.owner` → `users.username`
- `shot_prompts.owner` → `users.username`;`shot_prompts.category_id` → `prompt_categories.id`
- `publish_accounts.owner` → `users.username`;`publish_accounts.category_id` → `customers.id`

配置类表(`industries` / `prompts` / `providers` / `app_secrets`)无 `owner`,全员共用;`prompt_categories` / `shot_prompts` 虽属创作素材但按 `owner` 分归属。

---

## 遗留表(代码已移除,仅旧库存在)

以下两张表在 `crates/core/src/db/entity/` 中已无对应实体,`init_schema` 不再建表,仅存在于历史创建的库中。连同 24 张实体表,旧库全库合计 26 张表。

### publish_account_categories(发布账号分类表,遗留)

发布账号早期的独立分类表(用户真实库中尚有 1 行数据)。已被 `customers` 表替代:发布账号的 `category_id` 直接存 `customers.id`,客户在「运营 > 客户管理」统一维护,不再单独建分类表。新库不会创建本表;旧库中的数据需人工核对迁移后可删。

DDL(SQLite):

```sql
CREATE TABLE "publish_account_categories" (
  "id" varchar NOT NULL PRIMARY KEY,
  "name" varchar NOT NULL,
  "remark" text NOT NULL,
  "sort" bigint NOT NULL,
  "owner" varchar NOT NULL,
  "created_at" bigint NOT NULL
)
```

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | TEXT | 否 | 无 | 分类唯一 ID |
| name | TEXT | 否 | 无 | 分类名称 |
| remark | TEXT | 否 | 无 | 备注 |
| sort | BIGINT | 否 | 无 | 排序值 |
| owner | TEXT | 否 | 无 | 归属用户 |
| created_at | BIGINT | 否 | 无 | 创建时间(Unix 秒) |

### update_history(版本更新记录表,遗留)

早期记录应用版本升级历史的表(用户真实库中为 0 行)。当前版本号递增由 `bun run tauri build` 的构建脚本同步 `package.json` / `tauri.conf.json` / `Cargo.toml`,不再落库记录。新库不会创建本表,旧库可直接删除。

DDL(SQLite):

```sql
CREATE TABLE "update_history" (
  "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
  "from_version" varchar NOT NULL,
  "to_version" varchar NOT NULL,
  "notes" text,
  "updated_at" bigint NOT NULL
)
```

| 字段名 | 类型 | 可空 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| id **(PK)** | INTEGER | 否 | 自增 | 自增主键 |
| from_version | TEXT | 否 | 无 | 升级前版本号 |
| to_version | TEXT | 否 | 无 | 升级后版本号 |
| notes | TEXT | 是 | NULL | 升级说明 |
| updated_at | BIGINT | 否 | 无 | 升级时间(Unix 秒) |
