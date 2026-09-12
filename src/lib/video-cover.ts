// 视频封面抓帧:隐藏 <video> 加载后 seek 到 1s(或时长 10%,取小),drawImage 到 canvas
// 导出 JPEG dataURL(宽 480,质量 0.7,约 30~60KB,随草稿存 localStorage)。
// 失败(解码异常 / 超时 10s)返回 null,调用方静默回退图标占位。
export function captureVideoCover(url: string): Promise<string | null> {
  return new Promise((resolve) => {
    const v = document.createElement("video");
    v.muted = true;
    v.preload = "auto";
    let settled = false;
    const timer = setTimeout(() => done(null), 10_000);

    function done(result: string | null) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      // 释放解码资源
      v.removeAttribute("src");
      v.load();
      resolve(result);
    }

    v.onloadedmetadata = () => {
      const d = v.duration;
      v.currentTime = Number.isFinite(d) && d > 0 ? Math.min(1, d * 0.1) : 0;
    };
    v.onseeked = () => {
      try {
        const w = 480;
        if (!v.videoWidth) return done(null);
        const h = Math.round((v.videoHeight / v.videoWidth) * w);
        const canvas = document.createElement("canvas");
        canvas.width = w;
        canvas.height = h;
        const ctx = canvas.getContext("2d");
        if (!ctx) return done(null);
        ctx.drawImage(v, 0, 0, w, h);
        done(canvas.toDataURL("image/jpeg", 0.7));
      } catch {
        done(null);
      }
    };
    v.onerror = () => done(null);
    v.src = url;
  });
}
