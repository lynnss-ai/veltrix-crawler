//! JWT 接口鉴权:Claims、签发、以及受保护接口用的 Bearer 提取器。
//!
//! 密钥从环境变量 `VELTRIX_JWT_SECRET` 读取(禁止硬编码);缺失时生成临时随机密钥(仅开发)。

use axum::{extract::FromRequestParts, http::request::Parts};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use super::{ApiState, AppError};

/// token 有效期(秒):7 天。
const TOKEN_TTL_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// 用户标识(用户 id 或 username)。
    pub sub: String,
    /// 数据级别:all / self,用于业务接口的数据可见范围控制。
    pub scope: String,
    /// 过期时间(unix 秒)。
    pub exp: usize,
}

/// 读取 JWT 密钥:优先环境变量;缺失时生成临时随机密钥(重启后旧 token 失效)。
pub fn jwt_secret() -> Vec<u8> {
    match std::env::var("VELTRIX_JWT_SECRET") {
        Ok(s) if !s.trim().is_empty() => s.into_bytes(),
        _ => {
            tracing::warn!(
                "未设置 VELTRIX_JWT_SECRET,使用临时随机密钥(重启后旧 token 失效);生产务必配置"
            );
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            format!("dev-secret-{seed}").into_bytes()
        }
    }
}

/// 签发 token。
pub fn encode_token(secret: &[u8], sub: &str, scope: &str) -> Result<String, AppError> {
    let exp = (chrono::Utc::now().timestamp() + TOKEN_TTL_SECS) as usize;
    let claims = Claims {
        sub: sub.to_string(),
        scope: scope.to_string(),
        exp,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| AppError::Internal(format!("签发 token 失败: {e}")))
}

/// 受保护接口的提取器:从 `Authorization: Bearer <token>` 解析并校验 JWT。
/// handler 参数加上 `AuthUser(claims): AuthUser` 即开启鉴权,失败自动返回 401。
pub struct AuthUser(pub Claims);

impl FromRequestParts<ApiState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::Unauthorized("缺少 Authorization 头".into()))?;
        let token = header.strip_prefix("Bearer ").ok_or_else(|| {
            AppError::Unauthorized("Authorization 格式应为 Bearer <token>".into())
        })?;
        let data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(&state.jwt_secret),
            &Validation::default(),
        )
        .map_err(|e| AppError::Unauthorized(format!("token 无效或已过期: {e}")))?;
        Ok(AuthUser(data.claims))
    }
}
