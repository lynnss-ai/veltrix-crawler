//! 模型用量记录实体。每次 LLM 调用产生一条记录,用于账单统计与用量分析。

use sea_orm::entity::prelude::*;
use sea_orm::Set;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "model_usage_records")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 模型名(deepseek-chat, qwen-plus 等)
    pub model: String,
    /// 厂商 ID(providers.id)
    pub provider_id: String,
    /// 输入 token
    pub prompt_tokens: i64,
    /// 输出 token
    pub completion_tokens: i64,
    /// 合计 token
    pub total_tokens: i64,
    /// 来源: chat / agent_chat / coding / rpa / computer
    pub source: String,
    /// 归属用户
    pub owner: String,
    /// Unix 秒时间戳
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// 便捷插入:记录一次 LLM 调用的 token 用量。
    pub async fn record(
        db: &DatabaseConnection,
        model: &str,
        provider_id: &str,
        prompt: u32,
        completion: u32,
        source: &str,
        owner: &str,
    ) -> Result<(), DbErr> {
        let now = chrono::Utc::now().timestamp();
        ActiveModel {
            model: Set(model.to_string()),
            provider_id: Set(provider_id.to_string()),
            prompt_tokens: Set(prompt as i64),
            completion_tokens: Set(completion as i64),
            total_tokens: Set((prompt + completion) as i64),
            source: Set(source.to_string()),
            owner: Set(owner.to_string()),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok(())
    }
}
