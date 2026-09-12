// 音频波形峰值:fetch 本地文件 → WebAudio 解码 → 每桶峰值(归一化 0~1)。
// 模块级缓存(同一路径只解码一次);解码失败返回 null,调用方回退 CSS 纹理。
const cache = new Map<string, Promise<Float32Array | null>>();

export function getAudioPeaks(
  url: string,
  buckets: number,
): Promise<Float32Array | null> {
  let p = cache.get(url);
  if (!p) {
    p = computePeaks(url, buckets).catch(() => null);
    cache.set(url, p);
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
  const ctx = new AudioContext();
  try {
    const audio = await ctx.decodeAudioData(buf);
    const ch0 = audio.getChannelData(0);
    const ch1 = audio.numberOfChannels > 1 ? audio.getChannelData(1) : null;
    const n = ch0.length;
    const peaks = new Float32Array(buckets);
    const step = n / buckets;
    for (let b = 0; b < buckets; b++) {
      const from = Math.floor(b * step);
      const to = Math.min(n, Math.max(from + 1, Math.floor((b + 1) * step)));
      let peak = 0;
      for (let i = from; i < to; i++) {
        const v = Math.abs(ch1 ? (ch0[i] + ch1[i]) / 2 : ch0[i]);
        if (v > peak) peak = v;
      }
      peaks[b] = peak;
    }
    let max = 0;
    for (const v of peaks) if (v > max) max = v;
    if (max > 0) {
      for (let b = 0; b < buckets; b++) peaks[b] /= max;
    }
    return peaks;
  } finally {
    void ctx.close();
  }
}
