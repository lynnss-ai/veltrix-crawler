// 主题切换:直接点击在浅色 / 深色间切换(Sun/Moon 图标随主题过渡)。
import { Moon, Sun } from "lucide-react";
import { Button } from "@/components/ui/button";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { useTheme } from "@/components/theme-provider";

export function ModeToggle({ className }: { className?: string }) {
  const { theme, setTheme } = useTheme();
  const isDark = theme === "dark";

  return (
    <SimpleTooltip content={isDark ? "切换到浅色模式" : "切换到深色模式"}>
      <Button
        variant="ghost"
        size="icon"
        className={className}
        onClick={() => setTheme(isDark ? "light" : "dark")}
      >
        <Sun className="size-4 scale-100 rotate-0 transition-all dark:scale-0 dark:-rotate-90" />
        <Moon className="absolute size-4 scale-0 rotate-90 transition-all dark:scale-100 dark:rotate-0" />
        <span className="sr-only">切换主题</span>
      </Button>
    </SimpleTooltip>
  );
}
