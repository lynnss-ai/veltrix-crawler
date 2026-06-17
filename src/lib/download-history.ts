// 下载/导出历史记录(localStorage 持久化,前端独立维护;文件是否仍在本地由后端 path_exists 实时判)。
// 各导出动作(评论 Excel / 对话 MD / 对话 PDF / 代码 / 图表)成功落盘后调 recordDownload 记一条。

export interface DownloadRecord {
  id: string;
  /** 本地绝对路径 */
  path: string;
  /** 文件名(展示用) */
  name: string;
  /** 类型标签(评论导出 / 对话Markdown / 对话PDF / 代码 / 图表 等) */
  kind: string;
  /** 保存时间(Unix 毫秒) */
  savedAt: number;
}

const KEY = "veltrix.download-history";
// 最多保留条数,超出丢弃最旧的
const MAX_RECORDS = 50;
// 历史变更事件:标题栏的历史面板据此即时刷新
export const DOWNLOAD_HISTORY_EVENT = "veltrix-download-history-changed";

function read(): DownloadRecord[] {
  try {
    const raw = localStorage.getItem(KEY);
    return raw ? (JSON.parse(raw) as DownloadRecord[]) : [];
  } catch {
    return [];
  }
}

function write(list: DownloadRecord[]): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(list.slice(0, MAX_RECORDS)));
  } catch {
    // localStorage 写失败(隐私模式/超额)忽略,不影响导出本身
  }
  window.dispatchEvent(new Event(DOWNLOAD_HISTORY_EVENT));
}

/** 从绝对路径取文件名(兼容 Windows `\` 与 POSIX `/`) */
export function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** 记一条下载历史;同路径(覆盖保存)去重,新记录置顶 */
export function recordDownload(rec: {
  path: string;
  name?: string;
  kind: string;
}): void {
  if (!rec.path) return;
  const list = read().filter((r) => r.path !== rec.path);
  list.unshift({
    id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    path: rec.path,
    name: rec.name?.trim() || basename(rec.path),
    kind: rec.kind,
    savedAt: Date.now(),
  });
  write(list);
}

export function getDownloadHistory(): DownloadRecord[] {
  return read();
}

export function removeDownloadRecord(id: string): void {
  write(read().filter((r) => r.id !== id));
}

export function clearDownloadHistory(): void {
  write([]);
}
