use crate::{
    auth::{Auth, PERMISSIONS, audit, password_hash, validate_permissions, validate_username},
    error::{ApiError, ApiResult, reject_unknown, revision, text},
    net,
    state::App,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

#[derive(Default, Deserialize)]
pub struct ListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub search: Option<String>,
    pub entry_format_id: Option<String>,
    pub all_users: Option<bool>,
    pub include_deleted: Option<bool>,
}
impl ListQuery {
    pub fn paging(&self) -> (i64, i64) {
        let limit = self.per_page.unwrap_or(20).clamp(1, 100);
        (limit, (self.page.unwrap_or(1).clamp(1, 100000) - 1) * limit)
    }
    pub fn search(&self) -> String {
        format!(
            "%{}%",
            self.search
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(128)
                .collect::<String>()
        )
    }
}
pub async fn users(
    State(app): State<App>,
    auth: Auth,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    let (limit, offset) = query.paging();
    let rows=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(u) || jsonb_build_object('model_ids',COALESCE((SELECT jsonb_agg(model_id) FROM user_model_grants WHERE user_id=u.id),'[]'::jsonb)) FROM users u WHERE ($1 OR deleted_at IS NULL) AND (username ILIKE $2 OR display_name ILIKE $2) ORDER BY created_at DESC LIMIT $3 OFFSET $4").bind(query.include_deleted.unwrap_or(false)).bind(query.search()).bind(limit).bind(offset).fetch_all(&app.pool).await?;
    Ok(Json(
        json!({"items":rows,"page":offset/limit+1,"per_page":limit,"has_more":rows.len()==limit as usize}),
    ))
}
pub async fn user_detail(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    let value=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(u)||jsonb_build_object('model_ids',COALESCE((SELECT jsonb_agg(model_id) FROM user_model_grants WHERE user_id=u.id),'[]'::jsonb)) FROM users u WHERE id=$1").bind(id).fetch_optional(&app.pool).await?.ok_or_else(ApiError::missing)?;
    Ok(Json(value))
}
async fn save_user(app: &App, actor: Uuid, id: Option<Uuid>, body: Value) -> ApiResult<Value> {
    reject_unknown(
        &body,
        &[
            "username",
            "display_name",
            "password",
            "role",
            "status",
            "permissions",
            "model_access",
            "model_ids",
            "revision",
        ],
    )?;
    if id.is_some() && (body.get("username").is_some() || body.get("password").is_some()) {
        return Err(ApiError::validation("用户名不可更改；密码须单独重置"));
    }
    let prepared_password = if id.is_none() {
        Some(password_hash(text(&body, "password")?.to_string()).await?)
    } else {
        None
    };
    let mut transaction = app.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(734201)")
        .execute(&mut *transaction)
        .await?;
    let previous = if let Some(id) = id {
        sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(u) FROM users u WHERE id=$1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(ApiError::missing)?
    } else {
        json!({"role":"user","status":"enabled","permissions":PERMISSIONS,"model_access":"selected"})
    };
    let pick = |key: &str| body.get(key).unwrap_or(&previous[key]).clone();
    let role = pick("role");
    let status = pick("status");
    let access = pick("model_access");
    if !matches!(role.as_str(), Some("admin" | "user"))
        || !matches!(status.as_str(), Some("enabled" | "disabled"))
        || !matches!(access.as_str(), Some("selected" | "all_enabled"))
    {
        return Err(ApiError::validation("角色、状态或模型授权模式无效"));
    }
    let permissions = validate_permissions(&pick("permissions"))?;
    if id == Some(actor) && (role != "admin" || status != "enabled") {
        return Err(ApiError::validation(
            "不能在当前会话停用或降级自己，请另一名管理员操作",
        ));
    }
    if previous["role"] == "admin" && (role != "admin" || status != "enabled") {
        let remaining:i64=sqlx::query_scalar("SELECT count(*) FROM users WHERE role='admin' AND status='enabled' AND deleted_at IS NULL AND id<>$1").bind(id).fetch_one(&mut *transaction).await?;
        if remaining == 0 {
            return Err(ApiError::conflict(
                "LAST_ADMIN",
                "必须保留至少一名有效管理员",
            ));
        }
    }
    let display = pick("display_name");
    let display_name = display
        .as_str()
        .filter(|name| !name.trim().is_empty() && name.chars().count() <= 128)
        .ok_or_else(|| ApiError::validation("显示名须为 1–128 个字符"))?;
    let user_id = id.unwrap_or_else(Uuid::new_v4);
    if id.is_some() {
        let changed=sqlx::query("UPDATE users SET display_name=$1,role=$2,status=$3,permissions=$4,model_access=$5,revision=revision+1 WHERE id=$6 AND revision=$7").bind(display_name).bind(role.as_str()).bind(status.as_str()).bind(json!(permissions)).bind(access.as_str()).bind(user_id).bind(revision(&body)?).execute(&mut *transaction).await?.rows_affected();
        if changed == 0 {
            return Err(ApiError::conflict(
                "CONFLICT",
                "用户资料版本已改变，请重新加载",
            ));
        }
    } else {
        let username = validate_username(text(&body, "username")?)?;
        sqlx::query("INSERT INTO users(id,username,display_name,role,status,permissions,model_access) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(user_id).bind(&username).bind(display_name).bind(role.as_str()).bind(status.as_str()).bind(json!(permissions)).bind(access.as_str()).execute(&mut *transaction).await?;
        let identity_id = Uuid::new_v4();
        sqlx::query("INSERT INTO auth_identities(id,user_id,provider_id,subject) VALUES($1,$2,'local_password',$3)").bind(identity_id).bind(user_id).bind(username).execute(&mut *transaction).await?;
        sqlx::query("INSERT INTO local_credentials(identity_id,password_hash) VALUES($1,$2)")
            .bind(identity_id)
            .bind(prepared_password)
            .execute(&mut *transaction)
            .await?;
    }
    if let Some(model_ids) = body.get("model_ids") {
        let models: Vec<Uuid> = serde_json::from_value(model_ids.clone())
            .map_err(|_| ApiError::validation("模型集合无效"))?;
        if models.len() > 1000 {
            return Err(ApiError::validation("模型授权过多"));
        }
        sqlx::query("DELETE FROM user_model_grants WHERE user_id=$1")
            .bind(user_id)
            .execute(&mut *transaction)
            .await?;
        for model_id in models {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM model_configs WHERE id=$1 AND deleted_at IS NULL)",
            )
            .bind(model_id)
            .fetch_one(&mut *transaction)
            .await?;
            if !exists {
                return Err(ApiError::validation("模型配置不存在"));
            }
            sqlx::query("INSERT INTO user_model_grants VALUES($1,$2) ON CONFLICT DO NOTHING")
                .bind(user_id)
                .bind(model_id)
                .execute(&mut *transaction)
                .await?;
        }
    }
    if status != "enabled" {
        sqlx::query("UPDATE sessions SET revoked_at=now() WHERE user_id=$1")
            .bind(user_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    audit(
        app,
        actor,
        user_id,
        "user.save",
        json!({"role":role,"status":status}),
    )
    .await?;
    Ok(json!({"id":user_id,"message":"已保存；selected 空模型集合的用户当前没有可用模型"}))
}
pub async fn create_user(
    State(app): State<App>,
    auth: Auth,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    Ok(Json(save_user(&app, auth.user.id, None, body).await?))
}
pub async fn update_user(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    Ok(Json(save_user(&app, auth.user.id, Some(id), body).await?))
}
pub async fn delete_user(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    if id == auth.user.id {
        return Err(ApiError::validation("不能删除当前登录的管理员"));
    }
    let mut transaction = app.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(734201)")
        .execute(&mut *transaction)
        .await?;
    let row =
        sqlx::query("SELECT role,status FROM users WHERE id=$1 AND deleted_at IS NULL FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or_else(ApiError::missing)?;
    if row.get::<String, _>("role") == "admin" && row.get::<String, _>("status") == "enabled" {
        let remaining:i64=sqlx::query_scalar("SELECT count(*) FROM users WHERE role='admin' AND status='enabled' AND deleted_at IS NULL AND id<>$1").bind(id).fetch_one(&mut *transaction).await?;
        if remaining == 0 {
            return Err(ApiError::conflict(
                "LAST_ADMIN",
                "不能删除最后一名有效管理员",
            ));
        }
    }
    let changed = sqlx::query(
        "UPDATE users SET deleted_at=now(),revision=revision+1 WHERE id=$1 AND revision=$2",
    )
    .bind(id)
    .bind(revision(&body)?)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    if changed == 0 {
        return Err(ApiError::conflict("CONFLICT", "用户版本已改变"));
    }
    sqlx::query("UPDATE sessions SET revoked_at=now() WHERE user_id=$1")
        .bind(id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    audit(&app, auth.user.id, id, "user.delete", json!({})).await?;
    Ok(Json(json!({"ok":true})))
}
pub async fn reset_password(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    reject_unknown(&body, &["password", "revision"])?;
    let hash = password_hash(text(&body, "password")?.into()).await?;
    let mut transaction = app.pool.begin().await?;
    let changed = sqlx::query(
        "UPDATE users SET revision=revision+1 WHERE id=$1 AND revision=$2 AND deleted_at IS NULL",
    )
    .bind(id)
    .bind(revision(&body)?)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    if changed == 0 {
        return Err(ApiError::conflict("CONFLICT", "用户版本已改变或不存在"));
    }
    sqlx::query("UPDATE local_credentials SET password_hash=$1 WHERE identity_id IN (SELECT id FROM auth_identities WHERE user_id=$2 AND provider_id='local_password')").bind(hash).bind(id).execute(&mut *transaction).await?;
    sqlx::query("UPDATE sessions SET revoked_at=now() WHERE user_id=$1")
        .bind(id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    audit(&app, auth.user.id, id, "user.password_reset", json!({})).await?;
    Ok(Json(json!({"ok":true})))
}
pub const MODEL_PUBLIC: &str = "to_jsonb(m)-'credential_cipher'||jsonb_build_object('has_secret',credential_cipher IS NOT NULL,'secret_mask',CASE WHEN credential_cipher IS NULL THEN '' ELSE '••••••••' END)";
pub async fn models(
    State(app): State<App>,
    auth: Auth,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    let (limit, offset) = query.paging();
    let rows=sqlx::query_scalar::<_,Value>(&format!("SELECT {MODEL_PUBLIC} FROM model_configs m WHERE deleted_at IS NULL AND display_name ILIKE $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3")).bind(query.search()).bind(limit).bind(offset).fetch_all(&app.pool).await?;
    Ok(Json(
        json!({"items":rows,"page":offset/limit+1,"per_page":limit,"has_more":rows.len()==limit as usize}),
    ))
}
pub async fn model_detail(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    let model = sqlx::query_scalar::<_, Value>(&format!(
        "SELECT {MODEL_PUBLIC} FROM model_configs m WHERE id=$1 AND deleted_at IS NULL"
    ))
    .bind(id)
    .fetch_optional(&app.pool)
    .await?
    .ok_or_else(ApiError::missing)?;
    Ok(Json(model))
}
async fn save_model(app: &App, actor: Uuid, id: Option<Uuid>, body: Value) -> ApiResult<Value> {
    let _registry_guard = app.registry_lock.lock().await;
    reject_unknown(
        &body,
        &[
            "display_name",
            "entry_format_id",
            "upstream_model",
            "api_base_url",
            "credential",
            "credential_action",
            "capabilities",
            "defaults",
            "enabled",
            "revision",
        ],
    )?;
    let previous = if let Some(id) = id {
        sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(m) FROM model_configs m WHERE id=$1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(ApiError::missing)?
    } else {
        json!({"enabled":false,"capabilities":{"sizes":["1024x1024"],"input_mimes":["image/png","image/jpeg","image/webp"],"max_input_bytes":26214400,"max_pixels":40000000},"defaults":{}})
    };
    let pick = |key: &str| body.get(key).unwrap_or(&previous[key]).clone();
    let name = pick("display_name");
    let entry = pick("entry_format_id");
    let upstream = pick("upstream_model");
    let base = pick("api_base_url");
    for (field, value) in [
        ("display_name", &name),
        ("entry_format_id", &entry),
        ("upstream_model", &upstream),
        ("api_base_url", &base),
    ] {
        if value
            .as_str()
            .is_none_or(|text| text.trim().is_empty() || text.len() > 2048)
        {
            return Err(ApiError::validation(format!("{field} 无效")));
        }
    }
    let base = net::normalize_base(base.as_str().unwrap())?;
    let capabilities = pick("capabilities");
    reject_unknown(
        &capabilities,
        &["sizes", "input_mimes", "max_input_bytes", "max_pixels"],
    )?;
    if capabilities["input_mimes"].as_array().is_none_or(|mimes| {
        mimes.is_empty()
            || mimes.len() > 3
            || mimes.iter().any(|mime| {
                !matches!(
                    mime.as_str(),
                    Some("image/png" | "image/jpeg" | "image/webp")
                )
            })
    }) {
        return Err(ApiError::validation(
            "input_mimes 必须是 PNG/JPEG/WebP 类型数组",
        ));
    }
    let sizes = capabilities["sizes"]
        .as_array()
        .filter(|sizes| !sizes.is_empty() && sizes.len() <= 32)
        .ok_or_else(|| ApiError::validation("必须指定已验证支持的 sizes"))?;
    if sizes.iter().any(|size| {
        size.as_str().is_none_or(|size| {
            size.len() > 32
                || !size
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b':')
        })
    }) {
        return Err(ApiError::validation("尺寸格式无效"));
    }
    let max_bytes = capabilities["max_input_bytes"]
        .as_u64()
        .ok_or_else(|| ApiError::validation("max_input_bytes 须为正整数"))?;
    let max_pixels = capabilities["max_pixels"]
        .as_u64()
        .ok_or_else(|| ApiError::validation("max_pixels 须为正整数"))?;
    if max_bytes == 0 || max_bytes > 26214400 || max_pixels == 0 || max_pixels > 40000000 {
        return Err(ApiError::validation("文件限制不能超过平台上限"));
    }
    let action = body["credential_action"].as_str().unwrap_or("keep");
    let credential = match action {
        "keep" => previous["credential_cipher"].as_str().map(str::to_string),
        "clear" => None,
        "replace" => Some(
            app.config
                .encrypt(text(&body, "credential")?)
                .map_err(|_| ApiError::validation("密钥加密失败"))?,
        ),
        _ => {
            return Err(ApiError::validation(
                "credential_action 须为 keep/replace/clear",
            ));
        }
    };
    let enabled = pick("enabled")
        .as_bool()
        .ok_or_else(|| ApiError::validation("enabled 须为布尔值"))?;
    let snapshot = app.registry.load_full();
    let format = snapshot
        .entries
        .iter()
        .find(|format| format.id == entry.as_str().unwrap());
    if format.is_none() {
        return Err(ApiError::validation("入口尚未发布或不存在"));
    }
    if id.is_some() && entry != previous["entry_format_id"] {
        let inflight:i64=sqlx::query_scalar("SELECT count(*) FROM tasks WHERE model_id=$1 AND status NOT IN ('succeeded','failed','result_unavailable')").bind(id).fetch_one(&app.pool).await?;
        if inflight > 0 {
            return Err(ApiError::conflict(
                "PLUGIN_IN_USE",
                "有在途或待核对任务，不能替换入口格式",
            ));
        }
    }
    if enabled
        && credential.is_none()
        && format.unwrap().definition["request"]["authentication"] != "none"
    {
        return Err(ApiError::validation("启用模型需要配置凭据"));
    }
    let model_id = id.unwrap_or_else(Uuid::new_v4);
    let defaults = pick("defaults");
    if !defaults.is_object() {
        return Err(ApiError::validation("defaults 须为对象"));
    }
    reject_unknown(&defaults, &["size", "count"])?;
    if defaults
        .get("size")
        .is_some_and(|size| !sizes.contains(size))
        || defaults
            .get("count")
            .is_some_and(|count| count != &json!(1))
    {
        return Err(ApiError::validation(
            "默认 size 须属于支持尺寸，默认 count 仅接受 1",
        ));
    }
    if id.is_some() {
        let changed=sqlx::query("UPDATE model_configs SET display_name=$1,entry_format_id=$2,upstream_model=$3,api_base_url=$4,credential_cipher=$5,capabilities=$6,defaults=$7,enabled=$8,revision=revision+1,credential_epoch=credential_epoch+$9,updated_at=now() WHERE id=$10 AND revision=$11 AND deleted_at IS NULL").bind(name.as_str()).bind(entry.as_str()).bind(upstream.as_str()).bind(base).bind(credential).bind(capabilities).bind(defaults).bind(enabled).bind(if action=="keep" {0_i64}else{1}).bind(model_id).bind(revision(&body)?).execute(&app.pool).await?.rows_affected();
        if changed == 0 {
            return Err(ApiError::conflict("CONFLICT", "模型版本已改变"));
        }
    } else {
        sqlx::query("INSERT INTO model_configs(id,display_name,entry_format_id,upstream_model,api_base_url,credential_cipher,capabilities,defaults,enabled) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(model_id).bind(name.as_str()).bind(entry.as_str()).bind(upstream.as_str()).bind(base).bind(credential).bind(capabilities).bind(defaults).bind(enabled).execute(&app.pool).await?;
    }
    audit(
        app,
        actor,
        model_id,
        "model.save",
        json!({"credential_action":action}),
    )
    .await?;
    Ok(json!({"id":model_id,"message":"配置校验通过，尚未验证上游；真实测试可能计费"}))
}
pub async fn create_model(
    State(app): State<App>,
    auth: Auth,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    Ok(Json(save_model(&app, auth.user.id, None, body).await?))
}
pub async fn update_model(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    Ok(Json(save_model(&app, auth.user.id, Some(id), body).await?))
}
pub async fn delete_model(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    let changed=sqlx::query("UPDATE model_configs SET deleted_at=now(),enabled=false,revision=revision+1,credential_epoch=credential_epoch+1 WHERE id=$1 AND revision=$2 AND deleted_at IS NULL").bind(id).bind(revision(&body)?).execute(&app.pool).await?.rows_affected();
    if changed == 0 {
        return Err(ApiError::conflict("CONFLICT", "模型不存在或版本已改变"));
    }
    audit(&app, auth.user.id, id, "model.delete", json!({})).await?;
    Ok(Json(json!({"ok":true})))
}
