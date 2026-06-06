// 按窗口宽度自动收起/展开的侧栏状态。
// 跨过阈值时自动切换;用户手动切换会在下次跨阈值时被覆盖。

import { useEffect, useState } from "react";

export function useResponsiveCollapse(threshold = 1024) {
  const [collapsed, setCollapsed] = useState(
    typeof window !== "undefined" ? window.innerWidth < threshold : false,
  );
  useEffect(() => {
    const onResize = () => setCollapsed(window.innerWidth < threshold);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [threshold]);
  return [collapsed, setCollapsed] as const;
}
