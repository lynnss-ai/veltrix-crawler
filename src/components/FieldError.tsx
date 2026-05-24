// 字段级校验提示:在对应输入框下方显示错误信息(如「xxx 不可为空」)。
// 配合输入框的 aria-invalid 一起用,替代表单顶部的统一红框。
export function FieldError({
  show,
  message,
}: {
  show: boolean;
  message: string;
}) {
  if (!show) return null;
  return <p className="text-xs text-destructive">{message}</p>;
}
