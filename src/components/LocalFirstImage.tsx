// 本地优先图片:有本地路径走 asset 协议显示,加载失败回退平台外链,外链再失败隐藏。
// 封面与头像共用,避免多处重复 onError 回退逻辑。
import { convertFileSrc } from "@tauri-apps/api/core";

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
  const src = localPath ? convertFileSrc(localPath) : externalUrl;
  return (
    <img
      src={src}
      alt=""
      loading="lazy"
      // 异步解码:大图在后台线程解码,不阻塞滚动 / 切库时的主线程,与 loading=lazy 互补
      decoding="async"
      data-fallback={localPath ? externalUrl : ""}
      className={className}
      onClick={onClick}
      onError={(e) => {
        const img = e.currentTarget;
        const fb = img.dataset.fallback;
        if (fb && img.src !== fb) {
          img.dataset.fallback = "";
          img.src = fb;
        } else {
          img.style.display = "none";
        }
      }}
    />
  );
}
