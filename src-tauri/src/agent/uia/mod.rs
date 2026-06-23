//! 🪟 UI 控件读取工具模块(独立、可复用)。
//!
//! 区别于 `desktop`(按屏幕坐标盲操作)与 `fs`(磁盘):本模块用 Windows UI Automation 客户端,
//! 读**前台窗口**的控件树(名称 / 类型 / 可用 / 屏幕坐标),并按名称定位、Invoke 触发控件——
//! 让 Agent「看见」并语义化操作 GUI,而非靠像素坐标硬点。
//!
//! 工具:read_ui_tree(读控件树)/ find_control(按名找控件返坐标)/ click_control(找到并点击)。
//! 经 `tools::build_registry()` 拿到注册表后挂到任意 ReAct 循环。仅 Windows 实现,其它平台返回未实现提示。

pub mod tools;
