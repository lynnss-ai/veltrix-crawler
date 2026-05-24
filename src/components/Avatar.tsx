// 头像:有图(URL 或上传的 data URL)显示图片,否则用昵称/用户名首字母色块。
interface AvatarProps {
  src: string;
  name: string;
  size?: "sm" | "lg";
}

export function Avatar({ src, name, size = "sm" }: AvatarProps) {
  const dimension = size === "lg" ? "h-16 w-16" : "h-9 w-9";
  if (src) {
    return (
      <img
        src={src}
        alt={name}
        className={`${dimension} shrink-0 rounded-full object-cover`}
      />
    );
  }
  const initial = name.trim().charAt(0).toUpperCase() || "?";
  const fontSize = size === "lg" ? "text-xl" : "text-sm";
  return (
    <div
      className={`flex ${dimension} shrink-0 items-center justify-center rounded-full bg-indigo-500/15 ${fontSize} font-medium text-indigo-600 dark:text-indigo-300`}
    >
      {initial}
    </div>
  );
}
