//! JWT 接口鉴权:三套 Claims(用户 / PC / 手机),通过 `aud` 字段强隔离,防止 token 跨用。
//!
//! 密钥从环境变量 `VELTRIX_JWT_SECRET` 读取(禁止硬编码);缺失时生成临时随机密钥(仅开发)。
//!
//! 设计要点:
//! - UserClaims  — 桌面端/Web 登录后拿到,可调 /pair/init 等管理接口
//! - PcClaims    — 配对成功后云端给 PC 签发,用于上报设备状态(/devices/status)
//! - MobileClaims — 手机扫码绑定后签发,带 pair_id;每次请求由提取器查 Redis 比对当前绑定,
//!   实现「新手机绑定踢旧手机」语义

use axum::{extract::FromRequestParts, http::request::Parts};
use bb8_redis::redis::AsyncCommands;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use super::{ApiState, AppError};

const TOKEN_TTL_SECS: i64 = 7 * 24 * 3600;

const AUD_USER: &str = "user";
const AUD_PC: &str = "pc";
const AUD_MOBILE: &str = "mobile";

/// 用户(桌面端/Web 登录态)。
#[derive(Debug, Serialize, Deserialize)]
pub struct UserClaims {
    pub sub: String,
    /// 数据级别:all / self
    pub scope: String,
    pub aud: String,
    pub exp: usize,
}

/// PC 客户端(配对成功后云端签发,用于设备上报)。
#[derive(Debug, Serialize, Deserialize)]
pub struct PcClaims {
    /// sub = device_id
    pub sub: String,
    /// 配对发起的用户(便于审计)
    pub user: String,
    pub aud: String,
    pub exp: usize,
}

/// 手机端(扫码绑定后签发)。
#[derive(Debug, Serialize, Deserialize)]
pub struct MobileClaims {
    /// sub = 手机端用户标识(扫码时由云端生成的匿名 id,或后续接入手机账号体系)
    pub sub: String,
    /// 该手机绑定的 PC 设备 id
    pub device_id: String,
    /// 配对会话 id,云端 Redis 存当前合法值,值不一致即被踢
    pub pair_id: String,
    pub aud: String,
    pub exp: usize,
}

/// 解析 JWT 密钥。Cloud 模式必须显式配置,未配置直接 panic(避免误用随机密钥上线);
/// Desktop 模式允许回退到临时密钥(重启后旧 token 失效),便于本地开发。
pub fn jwt_secret(mode: super::ServerMode) -> Vec<u8> {
    match std::env::var("VELTRIX_JWT_SECRET") {
        Ok(s) if s.trim().len() >= 16 => s.into_bytes(),
        Ok(s) if !s.trim().is_empty() => {
            panic!(
                "VELTRIX_JWT_SECRET 长度不足 16 字符({}),拒绝启动",
                s.trim().len()
            );
        }
        _ => {
            if matches!(mode, super::ServerMode::Cloud) {
                panic!("Cloud 模式必须显式设置 VELTRIX_JWT_SECRET 环境变量");
            }
            tracing::warn!(
                "未设置 VELTRIX_JWT_SECRET,使用临时随机密钥(重启后旧 token 失效);Desktop 开发模式可接受"
            );
            // 用 CSPRNG 生成 32 字节高熵密钥:时间戳种子可被猜测,伪造 token 即绕过整套 API 鉴权
            use rand::RngCore;
            let mut secret = vec![0u8; 32];
            rand::thread_rng().fill_bytes(&mut secret);
            secret
        }
    }
}

fn exp_ts() -> usize {
    (chrono::Utc::now().timestamp() + TOKEN_TTL_SECS) as usize
}

fn encode_with<T: Serialize>(secret: &[u8], claims: &T) -> Result<String, AppError> {
    encode(&Header::default(), claims, &EncodingKey::from_secret(secret))
        .map_err(|e| AppError::Internal(format!("签发 token 失败: {e}")))
}

/// 签发用户 token(登录用)。
pub fn encode_user_token(secret: &[u8], sub: &str, scope: &str) -> Result<String, AppError> {
    encode_with(
        secret,
        &UserClaims {
            sub: sub.to_string(),
            scope: scope.to_string(),
            aud: AUD_USER.into(),
            exp: exp_ts(),
        },
    )
}

/// 签发 PC token(配对完成时给 PC 端)。
pub fn encode_pc_token(secret: &[u8], device_id: &str, user: &str) -> Result<String, AppError> {
    encode_with(
        secret,
        &PcClaims {
            sub: device_id.to_string(),
            user: user.to_string(),
            aud: AUD_PC.into(),
            exp: exp_ts(),
        },
    )
}

/// 签发手机 token(扫码确认时给手机端)。
pub fn encode_mobile_token(
    secret: &[u8],
    sub: &str,
    device_id: &str,
    pair_id: &str,
) -> Result<String, AppError> {
    encode_with(
        secret,
        &MobileClaims {
            sub: sub.to_string(),
            device_id: device_id.to_string(),
            pair_id: pair_id.to_string(),
            aud: AUD_MOBILE.into(),
            exp: exp_ts(),
        },
    )
}

// ---- 提取器:从 Bearer 头解析,并按 aud 强校验 ----

fn extract_bearer(parts: &Parts) -> Result<&str, AppError> {
    let header = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Unauthorized("缺少 Authorization 头".into()))?;
    header
        .strip_prefix("Bearer ")
        .ok_or_else(|| AppError::Unauthorized("Authorization 格式应为 Bearer <token>".into()))
}

fn validation_for(aud: &str) -> Validation {
    let mut v = Validation::default();
    // jsonwebtoken 默认会校验 exp;额外校验 aud 防止跨角色复用 token
    v.set_audience(&[aud]);
    v
}

fn decode_typed<T: for<'de> Deserialize<'de>>(
    token: &str,
    secret: &[u8],
    aud: &str,
) -> Result<T, AppError> {
    decode::<T>(token, &DecodingKey::from_secret(secret), &validation_for(aud))
        .map(|d| d.claims)
        .map_err(|e| AppError::Unauthorized(format!("token 无效或已过期: {e}")))
}

pub struct AuthUser(pub UserClaims);
pub struct AuthPc(pub PcClaims);
pub struct AuthMobile(pub MobileClaims);

impl FromRequestParts<ApiState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts)?;
        Ok(AuthUser(decode_typed::<UserClaims>(
            token,
            &state.jwt_secret,
            AUD_USER,
        )?))
    }
}

impl FromRequestParts<ApiState> for AuthPc {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts)?;
        Ok(AuthPc(decode_typed::<PcClaims>(
            token,
            &state.jwt_secret,
            AUD_PC,
        )?))
    }
}

impl FromRequestParts<ApiState> for AuthMobile {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts)?;
        let claims = decode_typed::<MobileClaims>(token, &state.jwt_secret, AUD_MOBILE)?;

        // 查 Redis 比对当前 device 绑定的 pair_id,被踢的旧手机这里会失败
        let redis = state.redis.as_ref().ok_or_else(|| {
            AppError::Internal("当前部署模式未启用 Redis,无法校验手机配对会话".into())
        })?;
        let mut conn = redis
            .get()
            .await
            .map_err(|e| AppError::Internal(format!("Redis 连接失败: {e}")))?;
        let key = format!("device:bind:{}", claims.device_id);
        let current: Option<String> = conn
            .get(&key)
            .await
            .map_err(|e| AppError::Internal(format!("Redis 查询失败: {e}")))?;
        match current {
            Some(pid) if pid == claims.pair_id => {
                // 滑动续期:每次使用都把绑定 TTL 拉满,30 天不再交互才会过期
                let _: Result<(), _> = conn.expire(&key, 30 * 24 * 3600).await;
                Ok(AuthMobile(claims))
            }
            Some(_) => Err(AppError::Unauthorized(
                "当前设备已被新手机绑定,旧会话失效".into(),
            )),
            None => Err(AppError::Unauthorized("设备未绑定或绑定已过期".into())),
        }
    }
}
