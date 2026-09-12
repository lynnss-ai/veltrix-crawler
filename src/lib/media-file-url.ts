import { convertFileSrc } from "@tauri-apps/api/core";
import { useSyncExternalStore } from "react";
import { api } from "@/lib/api";

// 素材路径口径:DB 新入库为相对 mediaRoot 的正斜杠相对路径(如 xhs/2026-09-08/image/xxx_cover.jpg);
// 存量数据可能仍是绝对路径。统一走本地文件服务(端口 8788,/files 前缀):
// 相对路径直接拼前缀;root 内的绝对路径剥离前缀;root 外的绝对路径(异常)回退 asset 协议直读。
// 前缀用 loopback(127.0.0.1):本机渲染不过网卡栈,也不必为取 LAN IP 启动 PowerShell;
// LAN 前缀(getFileServerPrefix)只留给「内网分享」场景(设置页展示/复制)。

// 缩略图路径推导:与源文件同目录,文件名去扩展名 + `_thumb.jpg`(文件服务对缺失的
// _thumb.jpg 惰性现生成,源图存在即不会 404)。无扩展名的路径原样返回(不构造必然 404 的地址)。
export function mediaThumbPath(path: string): string {
  const m = path.match(/^(.*)\.[a-z0-9]+$/i);
  // 扩展名前的最后一段不能含路径分隔符(避免把目录名里的点当扩展名)
  if (!m || m[1].endsWith("/")) return path;
  return `${m[1]}_thumb.jpg`;
}

type Resolver = (path: string) => string;
let resolve: Resolver = convertFileSrc;
let pending: Promise<void> | undefined;
let refreshedAt = 0;
const listeners = new Set<() => void>();
const refreshIfStale = () => {
  if (Date.now() - refreshedAt > 30_000) void refreshMediaFileUrls();
};
const subscribe = (listener: () => void) => {
  listeners.add(listener);
  if (listeners.size === 1) window.addEventListener("focus", refreshIfStale);
  refreshIfStale();
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) window.removeEventListener("focus", refreshIfStale);
  };
};

// 所有图片共享一次前缀/根目录查询;前缀走 loopback 命令(纯字符串拼装,不再触发网卡识别)。
export function refreshMediaFileUrls(): Promise<void> {
  if (pending) return pending;
  pending = Promise.all([api.getLocalFileServerPrefix(), api.getMediaRoot()])
    .then(([prefix, root]) => {
      const normalizedRoot = root.replace(/\\/g, "/").replace(/\/+$/, "");
      const windows = /^[a-z]:/i.test(normalizedRoot);
      resolve = (path) => {
        const normalized = path.replace(/\\/g, "/");
        // 相对路径(新入库口径):直接视为相对 mediaRoot;绝对路径(存量):剥离 root 前缀
        const isAbs = /^[a-z]:/i.test(normalized) || normalized.startsWith("/");
        const base = `${normalizedRoot}/`;
        let rel: string | null = null;
        if (!isAbs) {
          rel = normalized;
        } else if (windows
          ? normalized.toLowerCase().startsWith(base.toLowerCase())
          : normalized.startsWith(base)) {
          rel = normalized.slice(base.length);
        }
        // root 外的绝对路径(异常情况):回退 asset 协议直读
        if (rel === null) return convertFileSrc(path);
        const parts = rel.split("/");
        if (parts.some((part) => !part || part === "." || part === "..")) return convertFileSrc(path);
        // 无文件服务前缀时,相对路径拼回绝对再走 convertFileSrc
        if (!prefix) return convertFileSrc(isAbs ? path : `${normalizedRoot}/${rel}`);
        return `${prefix.replace(/\/+$/, "")}/${parts.map(encodeURIComponent).join("/")}`;
      };
    })
    .catch(() => { resolve = convertFileSrc; })
    .finally(() => {
      refreshedAt = Date.now();
      pending = undefined;
      listeners.forEach((listener) => listener());
    });
  return pending;
}

export function useMediaFileUrl(): Resolver {
  return useSyncExternalStore(subscribe, () => resolve);
}
