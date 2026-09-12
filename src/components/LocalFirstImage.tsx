// 本地优先图片:有本地路径走 asset 协议显示,加载失败回退平台外链,外链再失败隐藏。
// 封面与头像共用,避免多处重复 onError 回退逻辑。
import { useMediaFileUrl } from "@/lib/media-file-url";
import { useEffect, useRef, useState } from "react";

// 本地 URL 失败记忆:素材缺失的条目若不加记忆,每次渲染都先白等一次本地 404 才回退外链。
// 存失败时间戳而非纯集合——文件可能随后才下载好,超过 TTL 放行重试;
// 容量封顶防长会话内存膨胀(超额整体清空,条目少时重建代价可忽略)。
const LOCAL_FAIL_TTL_MS = 10 * 60_000;
const LOCAL_FAIL_CAP = 2000;
const failedLocalUrls = new Map<string, number>();

function isLocalKnownBad(url: string): boolean {
  const at = failedLocalUrls.get(url);
  if (at === undefined) return false;
  if (Date.now() - at > LOCAL_FAIL_TTL_MS) {
    failedLocalUrls.delete(url);
    return false;
  }
  return true;
}

function markLocalFailed(url: string) {
  if (failedLocalUrls.size >= LOCAL_FAIL_CAP) failedLocalUrls.clear();
  failedLocalUrls.set(url, Date.now());
}

export function LocalFirstImage({
  localPath,
  externalUrl,
  className,
  onClick,
}: {
  localPath: string | null;
  externalUrl: string;
  className: string;
  onClick?: () => void;
}) {
  const mediaFileUrl = useMediaFileUrl();
  const localUrl = localPath ? mediaFileUrl(localPath) : null;
  // 已知失败的本地地址直接跳外链,不再白等一次 404(TTL 过后自动放行重试)
  const preferredSrc =
    localUrl && !isLocalKnownBad(localUrl) ? localUrl : externalUrl;
  const imageRef = useRef<HTMLImageElement>(null);
  const [shouldLoad, setShouldLoad] = useState(false);
  const [candidate, setCandidate] = useState<string | null>(preferredSrc || null);
  const [loaded, setLoaded] = useState(false);

  // 只有进入视口附近才把真实地址交给 WebView。单写 loading="lazy" 时 WebView 仍可能
  // 提前拉取嵌套滚动容器里的全部图片,大图库首屏会同时占用网络与解码线程。
  useEffect(() => {
    const element = imageRef.current;
    if (!element || shouldLoad) return;
    if (!("IntersectionObserver" in window)) {
      setShouldLoad(true);
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setShouldLoad(true);
          observer.disconnect();
        }
      },
      { rootMargin: "320px" },
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [shouldLoad]);

  useEffect(() => {
    setCandidate(preferredSrc || null);
    setLoaded(false);
  }, [preferredSrc]);

  return (
    <img
      ref={imageRef}
      src={shouldLoad && candidate ? candidate : undefined}
      alt=""
      loading="lazy"
      fetchPriority="low"
      // 异步解码:大图在后台线程解码,不阻塞滚动 / 切库时的主线程,与 loading=lazy 互补
      decoding="async"
      className={`${className} bg-muted ${!loaded && candidate ? "animate-pulse" : ""}`}
      onClick={onClick}
      onLoad={() => setLoaded(true)}
      onError={(e) => {
        // 本地源失败:记入失败表,后续渲染直接走外链(含缩略图,同源同记)
        if (localUrl && candidate === localUrl) markLocalFailed(localUrl);
        if (localPath && externalUrl && candidate !== externalUrl) {
          setLoaded(false);
          setCandidate(externalUrl);
        } else {
          // 两个图源都失败时保留原尺寸占位,避免瀑布流重新排版跳动。
          e.currentTarget.removeAttribute("src");
          setCandidate(null);
        }
      }}
    />
  );
}
