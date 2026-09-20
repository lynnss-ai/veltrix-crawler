//! 提示词表(创作 - 提示词管理)。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "prompts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 业务编码(如 PRM-XXXX),系统生成。
    pub code: String,
    pub name: String,
    /// 提示词类型:image(图片)/ video(视频);旧数据为空串或 adapt。
    pub kind: String,
    #[sea_orm(column_type = "Text")]
    pub content: String,
    /// 提示词来源(下拉选项,如 opennana)。
    pub source: String,
    /// 来源数据 id(该提示词取材的内容 / 数据记录 id,自由关联)。
    pub source_data_id: String,
    /// 示例链接(图片或视频 URL)。
    #[sea_orm(column_type = "Text")]
    pub example: String,
    /// 适配模型数组,以 JSON 字符串存储(如 ["deepseek-v4-pro","qwen-max"])。
    #[sea_orm(column_type = "Text")]
    pub models: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
