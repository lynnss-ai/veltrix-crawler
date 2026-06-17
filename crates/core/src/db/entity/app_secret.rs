//! 密钥键值表:存 api_key 等敏感配置(api_key 不落配置文件,统一存数据库)。
//! key 为用途标识(如 intent_api_key / transcription_api_key),value 为明文密钥。
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "app_secrets")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    #[sea_orm(column_type = "Text")]
    pub value: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
