use crate::{
    error::{ApiError, ApiResult, reject_unknown, text},
    state::App,
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts, State},
    http::{HeaderMap, HeaderValue, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::net::SocketAddr;
use uuid::Uuid;

pub const PERMISSIONS: [&str; 3] = ["image.generate", "task.read_own", "asset.download_own"];
#[derive(Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub status: String,
    pub permissions: Vec<String>,
    pub model_access: String,
    pub model_ids: Vec<Uuid>,
    pub revision: i64,
}
impl User {
    pub fn admin(&self) -> ApiResult<()> {
        if self.role == "admin" {
            Ok(())
        } else {
            Err(ApiError::forbidden())
        }
    }
    pub fn can(&self, permission: &str) -> bool {
        self.role == "admin" || self.permissions.iter().any(|value| value == permission)
    }
    pub fn require(&self, permission: &str) -> ApiResult<()> {
        if self.can(permission) {
            Ok(())
        } else {
            Err(ApiError::forbidden())
        }
    }
    pub fn model(&self, id: Uuid) -> bool {
        self.role == "admin" || self.model_access == "all_enabled" || self.model_ids.contains(&id)
    }
}
pub async fn user_by_id(pool: &PgPool, id: Uuid) -> ApiResult<User> {
    let row =
        sqlx::query("SELECT * FROM users WHERE id=$1 AND status='enabled' AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(unauthorized)?;
    let permissions: Value = row.get("permissions");
    let model_ids =
        sqlx::query_scalar::<_, Uuid>("SELECT model_id FROM user_model_grants WHERE user_id=$1")
            .bind(id)
            .fetch_all(pool)
            .await?;
    Ok(User {
        id,
        username: row.get("username"),
        display_name: row.get("display_name"),
        role: row.get("role"),
        status: row.get("status"),
        permissions: serde_json::from_value(permissions).unwrap_or_default(),
        model_access: row.get("model_access"),
        model_ids,
        revision: row.get("revision"),
    })
}
pub fn hash(text: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(text.as_ref()))
}
pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}
pub fn unauthorized() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "UNAUTHENTICATED",
        "登录已过期，请重新登录",
    )
}
pub fn check_origin(app: &App, headers: &HeaderMap) -> ApiResult<()> {
    if headers.get("origin").and_then(|value| value.to_str().ok())
        != Some(app.config.origin.as_str())
    {
        return Err(ApiError::forbidden());
    }
    Ok(())
}
pub struct Auth {
    pub user: User,
    pub session_hash: String,
    pub csrf_token: String,
}
impl FromRequestParts<App> for Auth {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, app: &App) -> ApiResult<Self> {
        let token = parts
            .headers
            .get("cookie")
            .and_then(|value| value.to_str().ok())
            .and_then(|cookies| {
                cookies
                    .split(';')
                    .find_map(|cookie| cookie.trim().strip_prefix("newphoto_session="))
            })
            .ok_or_else(unauthorized)?;
        if token.len() > 128 {
            return Err(unauthorized());
        }
        let session_hash = hash(token);
        let row = sqlx::query("UPDATE sessions SET last_seen_at=now() WHERE token_hash=$1 AND revoked_at IS NULL AND expires_at>now() AND last_seen_at>now()-interval '2 hours' RETURNING user_id,csrf_token").bind(&session_hash).fetch_optional(&app.pool).await?.ok_or_else(unauthorized)?;
        let csrf_token: String = row.get("csrf_token");
        if !matches!(parts.method.as_str(), "GET" | "HEAD" | "OPTIONS") {
            check_origin(app, &parts.headers)?;
            if parts
                .headers
                .get("x-csrf-token")
                .and_then(|value| value.to_str().ok())
                != Some(csrf_token.as_str())
            {
                return Err(ApiError::forbidden());
            }
        }
        let user = user_by_id(&app.pool, row.get("user_id")).await?;
        Ok(Self {
            user,
            session_hash,
            csrf_token,
        })
    }
}

pub struct VerifiedIdentity {
    pub provider_id: &'static str,
    pub subject: String,
}
pub trait AuthProvider {
    fn id(&self) -> &'static str;
    fn supported_flows(&self) -> &'static [&'static str];
    async fn authenticate(
        &self,
        app: &App,
        username: String,
        password: String,
    ) -> ApiResult<VerifiedIdentity>;
    fn begin(&self) -> ApiResult<()> {
        Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "NOT_SUPPORTED",
            "此登录流程尚不支持",
        ))
    }
    fn complete(&self) -> ApiResult<VerifiedIdentity> {
        Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "NOT_SUPPORTED",
            "此登录流程尚不支持",
        ))
    }
}
pub struct LocalPasswordProvider;
impl AuthProvider for LocalPasswordProvider {
    fn id(&self) -> &'static str {
        "local_password"
    }
    fn supported_flows(&self) -> &'static [&'static str] {
        &["password"]
    }
    async fn authenticate(
        &self,
        app: &App,
        username: String,
        password: String,
    ) -> ApiResult<VerifiedIdentity> {
        let row = sqlx::query("SELECT c.password_hash FROM auth_identities i JOIN local_credentials c ON c.identity_id=i.id JOIN users u ON u.id=i.user_id WHERE i.provider_id=$1 AND i.subject=$2 AND u.status='enabled' AND u.deleted_at IS NULL").bind(self.id()).bind(&username).fetch_optional(&app.pool).await?;
        let stored = row
            .as_ref()
            .map(|row| row.get::<String, _>("password_hash"))
            .unwrap_or_else(|| app.dummy_password.clone());
        let verified = tokio::task::spawn_blocking(move || {
            PasswordHash::new(&stored).ok().is_some_and(|hash| {
                password.len() <= 512
                    && Argon2::default()
                        .verify_password(password.as_bytes(), &hash)
                        .is_ok()
            })
        })
        .await
        .unwrap_or(false);
        if !verified || row.is_none() {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "INVALID_CREDENTIALS",
                "用户名或密码错误",
            ));
        }
        Ok(VerifiedIdentity {
            provider_id: self.id(),
            subject: username,
        })
    }
}
pub struct IdentityResolver;
impl IdentityResolver {
    pub async fn resolve(pool: &PgPool, identity: VerifiedIdentity) -> ApiResult<Uuid> {
        sqlx::query_scalar(
            "SELECT user_id FROM auth_identities WHERE provider_id=$1 AND subject=$2",
        )
        .bind(identity.provider_id)
        .bind(identity.subject)
        .fetch_optional(pool)
        .await?
        .ok_or_else(unauthorized)
    }
}
pub fn provider_metadata() -> Value {
    let provider = LocalPasswordProvider;
    json!({"id":provider.id(),"flows":provider.supported_flows(),"redirect_supported":provider.begin().is_ok(),"challenge_supported":provider.complete().is_ok()})
}
pub fn validate_username(value: &str) -> ApiResult<String> {
    let normalized = value.to_ascii_lowercase();
    if !(3..=64).contains(&normalized.len())
        || !normalized.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        return Err(ApiError::validation(
            "用户名须为 3–64 位 ASCII 字母、数字、点、下划线或短横线",
        ));
    }
    Ok(normalized)
}
pub async fn password_hash(password: String) -> ApiResult<String> {
    if !(12..=128).contains(&password.chars().count()) {
        return Err(ApiError::validation(
            "密码须为 12–128 个字符，不会自动去除空格",
        ));
    }
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>())
            .map_err(|_| ApiError::validation("密码处理失败"))?;
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| ApiError::validation("密码处理失败"))
    })
    .await
    .map_err(|_| ApiError::validation("密码处理失败"))?
}
pub fn validate_permissions(value: &Value) -> ApiResult<Vec<String>> {
    let permissions: Vec<String> = serde_json::from_value(value.clone())
        .map_err(|_| ApiError::validation("权限必须为数组"))?;
    if permissions
        .iter()
        .any(|permission| !PERMISSIONS.contains(&permission.as_str()))
        || ((permissions.contains(&"image.generate".into())
            || permissions.contains(&"asset.download_own".into()))
            && !permissions.contains(&"task.read_own".into()))
    {
        return Err(ApiError::validation("权限无效；生成和下载权限依赖查看权限"));
    }
    Ok(permissions)
}
pub async fn audit(
    app: &App,
    actor: Uuid,
    target: impl ToString,
    category: &str,
    detail: Value,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO audit_logs(id,actor_id,target,category,detail) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(Uuid::new_v4())
    .bind(actor)
    .bind(target.to_string())
    .bind(category)
    .bind(detail)
    .execute(&app.pool)
    .await?;
    Ok(())
}
pub async fn login(
    State(app): State<App>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check_origin(&app, &headers)?;
    reject_unknown(&body, &["username", "password"])?;
    let username = body["username"].as_str().unwrap_or("").to_ascii_lowercase();
    let password = body["password"].as_str().unwrap_or("").to_string();
    for bucket in [
        format!("ip:{}", address.ip()),
        format!("user:{}", hash(&username)),
    ] {
        let attempts: i32 = sqlx::query_scalar("INSERT INTO login_limits(bucket,attempts) VALUES($1,1) ON CONFLICT(bucket) DO UPDATE SET attempts=CASE WHEN login_limits.window_start<now()-interval '15 minutes' THEN 1 ELSE login_limits.attempts+1 END, window_start=CASE WHEN login_limits.window_start<now()-interval '15 minutes' THEN now() ELSE login_limits.window_start END RETURNING attempts").bind(&bucket).fetch_one(&app.pool).await?;
        if attempts > if bucket.starts_with("ip:") { 60 } else { 12 } {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "尝试过于频繁，请 15 分钟后重试",
            ));
        }
    }
    let identity = LocalPasswordProvider
        .authenticate(&app, username, password)
        .await?;
    let user_id = IdentityResolver::resolve(&app.pool, identity).await?;
    let token = random_token();
    let csrf = random_token();
    sqlx::query("INSERT INTO sessions(token_hash,user_id,csrf_token) VALUES($1,$2,$3)")
        .bind(hash(&token))
        .bind(user_id)
        .bind(&csrf)
        .execute(&app.pool)
        .await?;
    sqlx::query("UPDATE users SET last_login_at=now() WHERE id=$1")
        .bind(user_id)
        .execute(&app.pool)
        .await?;
    let user = user_by_id(&app.pool, user_id).await?;
    let mut response = Json(json!({"user":user,"csrf_token":csrf})).into_response();
    let cookie = format!(
        "newphoto_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=28800{}",
        if app.config.secure_cookie {
            "; Secure"
        } else {
            ""
        }
    );
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&cookie).map_err(|_| unauthorized())?,
    );
    Ok(response)
}
pub async fn me(auth: Auth) -> Json<Value> {
    Json(json!({"user":auth.user,"csrf_token":auth.csrf_token}))
}
pub async fn logout(State(app): State<App>, auth: Auth) -> ApiResult<Response> {
    sqlx::query("UPDATE sessions SET revoked_at=now() WHERE token_hash=$1")
        .bind(auth.session_hash)
        .execute(&app.pool)
        .await?;
    let mut response = Json(json!({"ok":true})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_static("newphoto_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    Ok(response)
}
pub async fn update_me(
    State(app): State<App>,
    auth: Auth,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    reject_unknown(&body, &["display_name", "revision"])?;
    let affected = sqlx::query(
        "UPDATE users SET display_name=$1,revision=revision+1 WHERE id=$2 AND revision=$3",
    )
    .bind(text(&body, "display_name")?)
    .bind(auth.user.id)
    .bind(crate::error::revision(&body)?)
    .execute(&app.pool)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(ApiError::conflict("CONFLICT", "资料已更新，请刷新后重试"));
    }
    Ok(Json(
        json!({"user":user_by_id(&app.pool, auth.user.id).await?}),
    ))
}
