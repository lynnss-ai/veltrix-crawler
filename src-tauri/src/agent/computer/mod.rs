//! 🖥️ 电脑操作智能体:把 desktop/fs/system/ocr/uia/net/shell 七个独立工具模块**组装**成一个
//! 能跑的 Agent(`commands` 的 ReAct 循环 + `tools` 的工具聚合 / 提示词 / 危险工具判定)。
//! 横向能力来自各独立模块(仍可被别处复用),本模块只负责"装到一起"并驱动 ReAct。

pub mod commands;
pub mod recorder;
pub mod tools;
