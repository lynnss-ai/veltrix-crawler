// 表单控件统一尺寸:仅统一输入框 / 下拉触发器为 h-10。
// 通过作用域类挂到页面根容器及 Dialog/Sheet 内容区,使输入类控件高度一致。
// 按钮**不**在此统一——按钮一律用 Button 自身 size(默认 h-8),否则取消/确定等会被强拉成 h-10 显得过大。
// 例外:分页器(DataTablePagination)的「每页行数」下拉自带紧凑 h-7,标了 data-pagination,用 :not 排除。
export const FORM_CONTROL_SIZING =
  "[&_[data-slot=input]]:h-10 [&_[data-slot=select-trigger]:not([data-pagination])]:!h-10";
