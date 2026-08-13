// 采集数据 Excel 导出:「文案内容」+「评论」双 sheet,任务详情「执行历史 · 导出」
// 与任务调度「更多 → 导出」共用;文件路径经系统保存对话框选定。
import * as XLSX from "xlsx";
import { save } from "@tauri-apps/plugin-dialog";

import { api } from "@/lib/api";
import type { CommentView, ContentView } from "@/lib/api-types";
import { recordDownload } from "@/lib/download-history";
import { authorProfileUrl, contentDetailUrl } from "@/lib/platforms";
import { formatTimestamp } from "@/lib/utils";

// 表头样式:居中 + 靛蓝背景 + 加粗白字(与全量库/评论库导出一致)
const HEADER_STYLE = {
  font: { bold: true, color: { rgb: "FFFFFF" } },
  fill: { fgColor: { rgb: "4F46E5" } },
  alignment: { horizontal: "center" as const, vertical: "center" as const },
};

function buildSheet(rows: Record<string, unknown>[], widths: number[]) {
  const ws = XLSX.utils.json_to_sheet(rows);
  if (ws["!ref"]) {
    const range = XLSX.utils.decode_range(ws["!ref"]);
    for (let col = range.s.c; col <= range.e.c; col++) {
      const addr = XLSX.utils.encode_cell({ r: 0, c: col });
      const cell = ws[addr];
      if (cell) (cell as Record<string, unknown>).s = HEADER_STYLE;
    }
  }
  ws["!cols"] = widths.map((wch) => ({ wch }));
  return ws;
}

function contentRows(
  contents: ContentView[],
  platformName: (id: string) => string,
) {
  return contents.map((c) => ({
    平台: platformName(c.platform),
    行业: c.industry,
    视频ID: c.contentId,
    // 抖音等平台无独立标题(正文在 desc):标题列回退用 desc,简介列此时留空不重复
    标题: c.title ?? c.desc ?? "",
    简介: c.title ? (c.desc ?? "") : "",
    作者: c.authorNickname,
    点赞数: c.likeCount ?? 0,
    评论数: c.commentCount ?? 0,
    收藏数: c.collectCount ?? 0,
    分享数: c.shareCount ?? 0,
    采集关键词: c.keyword,
    文案: c.transcript ?? "",
    内容链接: contentDetailUrl(c.platform, c.contentId) ?? "",
    发布时间: formatTimestamp(c.publishedAt),
    采集时间: formatTimestamp(c.collectedAt),
  }));
}

function commentRows(
  comments: CommentView[],
  platformName: (id: string) => string,
) {
  return comments.map((c) => ({
    平台: platformName(c.platform),
    视频ID: c.contentId,
    评论者: c.authorNickname,
    作者主页: authorProfileUrl(c.platform, c.authorUid, c.authorUniqueId) ?? "",
    评论内容: c.text,
    点赞数: c.likeCount ?? 0,
    回复数: c.replyCount ?? 0,
    评论时间: formatTimestamp(c.createdAt),
    采集时间: formatTimestamp(c.collectedAt),
  }));
}

// 把采集数据(内容 + 评论)写成双 sheet 的 xlsx 并弹系统保存对话框落盘。
// 返回 true=已保存,false=用户取消。调用方负责空数据校验与结果提示。
export async function exportTaskDataExcel({
  contents,
  comments,
  platformName,
  fileName,
  kind,
}: {
  contents: ContentView[];
  comments: CommentView[];
  platformName: (id: string) => string;
  // 默认文件名(保存对话框可改)
  fileName: string;
  // 下载历史分类(「运行导出」/「任务导出」)
  kind: string;
}): Promise<boolean> {
  const wb = XLSX.utils.book_new();
  if (contents.length > 0) {
    XLSX.utils.book_append_sheet(
      wb,
      buildSheet(
        contentRows(contents, platformName),
        [8, 12, 22, 30, 36, 16, 8, 8, 8, 8, 18, 60, 46, 20, 20],
      ),
      "文案内容",
    );
  }
  if (comments.length > 0) {
    XLSX.utils.book_append_sheet(
      wb,
      buildSheet(commentRows(comments, platformName), [8, 22, 16, 42, 50, 8, 8, 20, 20]),
      "评论",
    );
  }
  const base64 = XLSX.write(wb, { type: "base64", bookType: "xlsx" });
  const path = await save({
    defaultPath: fileName,
    filters: [{ name: "Excel 工作簿", extensions: ["xlsx"] }],
  });
  if (!path) return false; // 用户取消保存
  await api.saveBinaryFile(path, base64);
  recordDownload({ path, name: fileName, kind });
  return true;
}
