// 软件自动更新:检查 GitHub Releases 的 latest.json,验签后下载安装并重启。
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { ask } from "@tauri-apps/plugin-dialog";
import { getVersion } from "@tauri-apps/api/app";
import { toast } from "sonner";

// 更新源开关:tauri.conf.json 的 updater endpoint 还是占位地址(OWNER/REPO)时保持 false,
// 跳过一切检查——否则每次检查都对占位地址发请求,后端刷
// 「update endpoint did not respond with a successful status code」错误日志。
// 将来发布正式更新渠道时:配好 tauri.conf.json 的 endpoints + pubkey,再把这里置 true。
const UPDATE_ENABLED = false;

/**
 * 检查并(经用户确认后)下载安装更新。
 * @param silent true=启动后台静默检查(无更新/失败都不打扰);false=手动检查(始终给反馈)
 */
export async function checkForUpdate(silent: boolean): Promise<void> {
  if (!UPDATE_ENABLED) {
    if (!silent) toast.info("当前版本未配置自动更新");
    return;
  }
  let update;
  try {
    update = await check();
  } catch (e) {
    // 拉取 latest.json 失败:更新源未配置(占位 endpoint)、网络不通、或尚无发布版本。
    // 静默检查不打扰;手动检查给友好提示,技术细节进 console 便于排查。
    console.error("检查更新失败:", e);
    if (!silent) toast.error("暂时无法连接更新服务器,请稍后再试");
    return;
  }

  if (!update) {
    if (!silent) toast.success("当前已是最新版本");
    return;
  }

  const detail = update.body ? `\n\n更新说明:\n${update.body}` : "";
  const confirmed = await ask(
    `发现新版本 ${update.version}(当前 ${update.currentVersion})。${detail}\n\n是否现在下载并安装?安装完成后会重启应用。`,
    { title: "发现新版本", kind: "info", okLabel: "下载并安装", cancelLabel: "稍后" },
  );
  if (!confirmed) return;

  const toastId = toast.loading(`正在下载 ${update.version}…`);
  try {
    let downloaded = 0;
    let total = 0;
    await update.downloadAndInstall((event) => {
      switch (event.event) {
        case "Started":
          total = event.data.contentLength ?? 0;
          break;
        case "Progress":
          downloaded += event.data.chunkLength;
          if (total > 0) {
            const pct = Math.round((downloaded / total) * 100);
            toast.loading(`下载中 ${pct}%`, { id: toastId });
          }
          break;
        case "Finished":
          toast.loading("安装中…", { id: toastId });
          break;
      }
    });
    toast.success("更新完成,即将重启应用", { id: toastId });
    await relaunch();
  } catch (e) {
    toast.error(`更新失败: ${e}`, { id: toastId });
  }
}

/** 取当前应用版本号(显示用);失败返回空串。 */
export async function currentVersion(): Promise<string> {
  try {
    return await getVersion();
  } catch {
    return "";
  }
}
