import { useEffect, useRef, type ReactNode } from "react";

export function AutoScrollList({
  count,
  children,
}: {
  count: number;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  // 用 ref 存暂停态:onMouseEnter/Leave 改它,rAF 循环里读它,避免重建动画
  const pausedRef = useRef(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    let raf = 0;
    let bottomFrames = 0; // 到底后停留的帧数,停一会再回顶,避免突兀
    const tick = () => {
      if (el && !pausedRef.current && el.scrollHeight > el.clientHeight) {
        if (el.scrollTop >= el.scrollHeight - el.clientHeight - 1) {
          bottomFrames += 1;
          if (bottomFrames > 90) {
            el.scrollTop = 0; // 到底停约 1.5s 后回到顶部
            bottomFrames = 0;
          }
        } else {
          el.scrollTop += 0.4;
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [count]);
  return (
    <div
      ref={ref}
      onMouseEnter={() => {
        pausedRef.current = true;
      }}
      onMouseLeave={() => {
        pausedRef.current = false;
      }}
      className="veltrix-no-scrollbar h-80 overflow-y-auto"
    >
      {children}
    </div>
  );
}
