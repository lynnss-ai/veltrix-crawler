// 音频波形峰值:fetch 本地文件 → WebAudio 解码 → 每桶峰值(归一化 0~1)。
// 模块级缓存(同一路径只解码一次);解码失败返回 null,调用方回退 CSS 纹理。
const PEAK_ALGORITHM_VERSION = 2;
const cache = new Map<string, Promise<Float32Array | null>>();

export function getAudioPeaks(
  url: string,
  buckets: number,
): Promise<Float32Array | null> {
  const key = `${PEAK_ALGORITHM_VERSION}:${buckets}:${url}`;
  let p = cache.get(key);
  if (!p) {
    p = computePeaks(url, buckets).catch(() => null);
    cache.set(key, p);
  }
  return p;
}

async function computePeaks(
  url: string,
  buckets: number,
): Promise<Float32Array | null> {
  const resp = await fetch(url);
  if (!resp.ok) return null;
  const buf = await resp.arrayBuffer();
  // 低采样率 OfflineAudioContext 解码:decodeAudioData 会重采样到 context 采样率,
  // 8kHz 对波形峰值绰绰有余,长视频解码内存比 48k 立体声低一个量级
  const ctx = new OfflineAudioContext(1, 1, 8000);
  const audio = await ctx.decodeAudioData(buf);
  const channels = Array.from({ length: audio.numberOfChannels }, (_, index) => audio.getChannelData(index));
  const n = channels[0]?.length ?? 0;
  if (!n) return null;
  const peaks = new Float32Array(buckets);
  const step = n / buckets;
  for (let b = 0; b < buckets; b++) {
    const from = Math.floor(b * step);
    const to = Math.min(n, Math.max(from + 1, Math.floor((b + 1) * step)));
    let peak = 0;
    for (let i = from; i < to; i++) {
      // 不平均左右声道:反相立体声相加会把真实声音抵消成“静音”波形。
      for (const channel of channels) peak = Math.max(peak, Math.abs(channel[i]));
    }
    peaks[b] = peak;
  }
  // 用 98 分位而非单个最大尖峰做显示归一化,避免一次爆音压扁整段人声波形。
  const sorted = Array.from(peaks).filter((value) => value > 0).sort((a, b) => a - b);
  const ceiling = sorted.length
    ? sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.98))]
    : 0;
  if (ceiling > 0) {
    for (let b = 0; b < buckets; b++) peaks[b] = Math.min(1, peaks[b] / ceiling);
  }
  return peaks;
}
