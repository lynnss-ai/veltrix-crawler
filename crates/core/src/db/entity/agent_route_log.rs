//! Agent 路由遥测实体。每条新会话首条消息的意图路由决策记一条。
//! 用于长期分析「哪些输入老路由错」,作为调关键词 / description / 上分层的依据
//! ——没有这层数据,路由优化全靠拍脑袋。

use sea_orm::entity::prelude::*;
use sea_orm::Set;

/// 首条消息文本入库前的截断上限(防长文案塞爆遥测表)。
const TEXT_CAP: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "agent_route_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 用户首条消息(截断,仅留前若干字供人工核对)
    #[sea_orm(column_type = "Text")]
    pub text: String,
    /// 关键词启发式得到的路由(chat/coding/rpa/computer/local)
    pub keyword_route: String,
    /// 是否触发了 LLM 兜底分类(仅关键词落到 chat 且像可执行任务时才触发)
    pub llm_used: bool,
    /// LLM 兜底给出的路由(未触发为空串)
    pub llm_route: String,
    /// 最终返回的路由
    pub final_route: String,
    /// LLM 兜底所用模型(未触发为空串)
    pub model: String,
    /// 归属用户
    pub owner: String,
    /// Unix 秒时间戳
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// 便捷插入:记录一次意图路由决策。`llm_route` 为 None 表示纯关键词命中、未走 LLM。
    pub async fn record(
        db: &DatabaseConnection,
        owner: &str,
        text: &str,
        keyword_route: &str,
        llm_route: Option<&str>,
        final_route: &str,
        model: &str,
    ) -> Result<(), DbErr> {
        let now = chrono::Utc::now().timestamp();
        let clipped: String = text.chars().take(TEXT_CAP).collect();
        ActiveModel {
            text: Set(clipped),
            keyword_route: Set(keyword_route.to_string()),
            llm_used: Set(llm_route.is_some()),
            llm_route: Set(llm_route.unwrap_or("").to_string()),
            final_route: Set(final_route.to_string()),
            model: Set(model.to_string()),
            owner: Set(owner.to_string()),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok(())
    }
}
