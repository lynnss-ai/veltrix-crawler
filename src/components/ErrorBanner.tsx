// 统一的错误提示条,可点击关闭。
interface ErrorBannerProps {
  message: string | null;
  onClose: () => void;
}

export function ErrorBanner({ message, onClose }: ErrorBannerProps) {
  if (!message) return null;
  return (
    <div className="mb-4 flex items-start justify-between rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">
      <span className="whitespace-pre-wrap">{message}</span>
      <button
        onClick={onClose}
        className="ml-4 shrink-0 text-destructive/70 transition-colors hover:text-destructive"
        aria-label="关闭"
      >
        ✕
      </button>
    </div>
  );
}
