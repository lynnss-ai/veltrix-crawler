// 表单控件统一尺寸:输入框 / 下拉触发器 / 文字按钮统一 h-10,文字按钮再加 px-4。
// 通过作用域类挂到页面根容器,页内所有控件随之统一(图标按钮 data-size^=icon 不受影响)。
// 例外:分页器(DataTablePagination)的「每页行数」下拉自带紧凑 h-7,要与翻页图标按钮等高;
//       它标了 data-pagination,用 :not 排除,否则会被强拉成 h-10,与同排翻页按钮高度不一致。
// 注意:Sheet / Dialog 经 portal 渲染,不在页面根内,故其内部控件不受此作用域影响。
export const FORM_CONTROL_SIZING =
  "[&_[data-slot=input]]:h-10 [&_[data-slot=select-trigger]:not([data-pagination])]:!h-10 [&_[data-slot=button]:not([data-size^=icon])]:h-10 [&_[data-slot=button]:not([data-size^=icon])]:px-4";
