// 全局错误边界:任一子组件渲染时抛错都被捕获,显示友好回退 UI 而非整页空白。
// 主要防御 hooks 顺序错乱、第三方库异常、数据格式不符等运行时崩溃。

import { Component, type ErrorInfo, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { TriangleAlert } from "lucide-react";

interface State {
  error: Error | null;
  componentStack: string;
}

interface Props {
  children: ReactNode;
  /// 可选:自定义回退 UI(默认显示通用错误页)
  fallback?: (error: Error, reset: () => void) => ReactNode;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, componentStack: "" };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // 控制台保留完整堆栈,方便开发期排查;生产可接入 Sentry / 日志服务
    console.error("[ErrorBoundary]", error, info);
    this.setState({ componentStack: info.componentStack ?? "" });
  }

  reset = () => {
    this.setState({ error: null, componentStack: "" });
  };

  render() {
    const { error, componentStack } = this.state;
    if (!error) return this.props.children;

    if (this.props.fallback) return this.props.fallback(error, this.reset);

    return (
      <div className="flex min-h-svh items-center justify-center bg-background p-6">
        <div className="w-full max-w-lg space-y-4 rounded-xl border bg-card p-6 shadow-sm">
          <div className="flex items-center gap-3">
            <div className="flex size-10 items-center justify-center rounded-full bg-destructive/10 text-destructive">
              <TriangleAlert className="size-5" />
            </div>
            <div>
              <h2 className="text-lg font-semibold text-foreground">出错了</h2>
              <p className="text-xs text-muted-foreground">
                页面渲染遇到异常,可重试或返回上一页
              </p>
            </div>
          </div>

          <div className="rounded-md border bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
            <div className="font-mono text-destructive">{error.message}</div>
            {componentStack && (
              <details className="mt-2">
                <summary className="cursor-pointer text-[11px]">
                  组件堆栈
                </summary>
                <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap text-[11px] leading-relaxed">
                  {componentStack.trim()}
                </pre>
              </details>
            )}
          </div>

          <div className="flex justify-end gap-2">
            <Button
              variant="outline"
              className="cursor-pointer"
              onClick={() => window.location.reload()}
            >
              重新加载
            </Button>
            <Button className="cursor-pointer" onClick={this.reset}>
              重试
            </Button>
          </div>
        </div>
      </div>
    );
  }
}
