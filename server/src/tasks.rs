use crate::{
    admin::ListQuery,
    auth::{Auth, audit, hash},
    error::{ApiError, ApiResult, reject_unknown, text},
    state::App,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

pub async fn create(
    State(app): State<App>,
    auth: Auth,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    create_inner(&app, &auth, &headers, body).await
}
pub async fn create_inner(
    app: &App,
    auth: &Auth,
    headers: &HeaderMap,
    body: Value,
) -> ApiResult<(StatusCode, Json<Value>)> {
    auth.user.require("image.generate")?;
    auth.user.require("task.read_own")?;
    reject_unknown(
        &body,
        &[
            "entry_format_id",
            "model_config_id",
            "model_revision",
            "registry_revision",
            "prompt",
            "input_asset_ids",
            "output",
            "parent_task_id",
        ],
    )?;
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|key| !key.is_empty() && key.len() <= 128)
        .ok_or_else(|| ApiError::validation("必须提供有效 Idempotency-Key"))?;
    let request_hash = hash(serde_json::to_vec(&body).unwrap());
    let mut transaction = app.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("{}:{key}", auth.user.id))
        .execute(&mut *transaction)
        .await?;
    if let Some(existing) = sqlx::query(
        "SELECT id,request_hash,status FROM tasks WHERE user_id=$1 AND idempotency_key=$2",
    )
    .bind(auth.user.id)
    .bind(key)
    .fetch_optional(&mut *transaction)
    .await?
    {
        if existing.get::<String, _>("request_hash") != request_hash {
            return Err(ApiError::conflict("CONFLICT", "同一幂等键已用于不同请求"));
        }
        return Ok((
            StatusCode::ACCEPTED,
            Json(
                json!({"task_id":existing.get::<Uuid,_>("id"),"status":existing.get::<String,_>("status"),"replayed":true}),
            ),
        ));
    }
    let paused: Value =
        sqlx::query_scalar("SELECT value FROM app_settings WHERE key='generation_paused'")
            .fetch_one(&mut *transaction)
            .await?;
    if paused == true {
        return Err(ApiError::conflict(
            "GENERATION_PAUSED",
            "恢复核对模式已暂停外部生成，请管理员确认后恢复",
        ));
    }
    let snapshot = app.registry.load_full();
    if body["registry_revision"].as_i64() != Some(snapshot.revision) {
        return Err(ApiError::conflict(
            "REGISTRY_CHANGED",
            "入口注册表已改变，请重新校验草稿",
        ));
    }
    let entry = snapshot
        .entries
        .iter()
        .find(|entry| Some(entry.id.as_str()) == body["entry_format_id"].as_str())
        .ok_or_else(|| ApiError::conflict("MODEL_UNAVAILABLE", "入口未发布或已停用"))?;
    let model_id: Uuid = text(&body, "model_config_id")?
        .parse()
        .map_err(|_| ApiError::validation("模型 ID 无效"))?;
    if !auth.user.model(model_id) {
        return Err(ApiError::forbidden());
    }
    let model=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(m)-'credential_cipher' FROM model_configs m WHERE id=$1 AND enabled AND deleted_at IS NULL FOR SHARE").bind(model_id).fetch_optional(&mut *transaction).await?.ok_or_else(||ApiError::conflict("MODEL_UNAVAILABLE","模型不可用"))?;
    if model["entry_format_id"] != entry.id {
        return Err(ApiError::validation("入口格式与模型不匹配"));
    }
    if body["model_revision"] != model["revision"] {
        return Err(ApiError::conflict(
            "MODEL_UNAVAILABLE",
            "模型配置版本已改变，请重新选择",
        ));
    }
    let prompt = text(&body, "prompt")?;
    if prompt.chars().count() > 4000 {
        return Err(ApiError::validation("提示词超过 4000 字符"));
    }
    reject_unknown(&body["output"], &["size", "count"])?;
    if body["output"]["count"] != 1 {
        return Err(ApiError::validation("V1 count 只接受 1"));
    }
    if !model["capabilities"]["sizes"]
        .as_array()
        .is_some_and(|sizes| sizes.contains(&body["output"]["size"]))
    {
        return Err(ApiError::validation("输出尺寸不在部署支持范围"));
    }
    let input_ids: Vec<Uuid> =
        serde_json::from_value(body.get("input_asset_ids").cloned().unwrap_or(json!([])))
            .map_err(|_| ApiError::validation("原图 ID 数组无效"))?;
    if entry.operation == "edit" && input_ids.len() != 1
        || entry.operation == "generate" && !input_ids.is_empty()
    {
        return Err(ApiError::validation("编辑必须提供一张原图；生成不接受原图"));
    }
    for input_id in input_ids {
        let input=sqlx::query("SELECT mime,byte_size,width,height FROM assets WHERE id=$1 AND user_id=$2 AND kind='input'").bind(input_id).bind(auth.user.id).fetch_optional(&mut *transaction).await?.ok_or_else(ApiError::missing)?;
        let mime: String = input.get("mime");
        let size: i64 = input.get("byte_size");
        let pixels =
            i64::from(input.get::<i32, _>("width")) * i64::from(input.get::<i32, _>("height"));
        if !model["capabilities"]["input_mimes"]
            .as_array()
            .is_some_and(|mimes| mimes.contains(&json!(mime)))
            || size
                > model["capabilities"]["max_input_bytes"]
                    .as_i64()
                    .unwrap_or(26214400)
            || pixels
                > model["capabilities"]["max_pixels"]
                    .as_i64()
                    .unwrap_or(40000000)
        {
            return Err(ApiError::validation("原图不符合模型的格式、体积或像素限制"));
        }
    }
    let parent: Option<Uuid> = body["parent_task_id"]
        .as_str()
        .map(str::parse)
        .transpose()
        .map_err(|_| ApiError::validation("关联任务 ID 无效"))?;
    if let Some(parent) = parent {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tasks WHERE id=$1 AND user_id=$2)")
                .bind(parent)
                .bind(auth.user.id)
                .fetch_one(&mut *transaction)
                .await?;
        if !exists {
            return Err(ApiError::missing());
        }
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tasks(id,user_id,idempotency_key,request_hash,request,model_id,model_snapshot,entry_snapshot,registry_revision,status,parent_task_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'queued',$10)").bind(id).bind(auth.user.id).bind(key).bind(request_hash).bind(body).bind(model_id).bind(model).bind(json!(entry)).bind(snapshot.revision).bind(parent).execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"task_id":id,"status":"queued","detail_url":format!("/api/v1/tasks/{id}")})),
    ))
}
const TASK_VIEW: &str = "to_jsonb(t)-'model_snapshot'-'entry_snapshot'-'request_hash'-'lease_token'-'lease_until'||jsonb_build_object('model_display_name',model_snapshot->>'display_name','entry_format_id',entry_snapshot->>'id','entry_version',entry_snapshot->>'version','entry_digest',entry_snapshot->>'digest','elapsed_ms',COALESCE(total_ms,GREATEST(0,(extract(epoch FROM (now()-t.created_at))*1000)::bigint)),'latest_upstream',(SELECT jsonb_build_object('phase',phase,'http_status',http_status,'transport_error',transport_error,'summary',summary) FROM upstream_requests WHERE task_id=t.id ORDER BY started_at DESC LIMIT 1),'assets',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',a.id,'mime',a.mime,'width',a.width,'height',a.height)) FROM assets a WHERE a.task_id=t.id AND a.kind='result'),'[]'::jsonb))";
pub async fn authorize(app: &App, auth: &Auth, id: Uuid) -> ApiResult<()> {
    let owner: Uuid = sqlx::query_scalar("SELECT user_id FROM tasks WHERE id=$1")
        .bind(id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(ApiError::missing)?;
    if owner != auth.user.id && auth.user.role != "admin" {
        return Err(ApiError::missing());
    }
    auth.user.require("task.read_own")?;
    if owner != auth.user.id {
        audit(app, auth.user.id, id, "task.admin_read", json!({})).await?;
    }
    Ok(())
}
pub async fn list(
    State(app): State<App>,
    auth: Auth,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    auth.user.require("task.read_own")?;
    let all = query.all_users.unwrap_or(false);
    if all {
        auth.user.admin()?;
        audit(&app, auth.user.id, "tasks", "task.admin_list", json!({})).await?;
    }
    let (limit, offset) = query.paging();
    let rows=sqlx::query_scalar::<_,Value>(&format!("SELECT {TASK_VIEW}||jsonb_build_object('username',u.username) FROM tasks t JOIN users u ON u.id=t.user_id WHERE ($1 OR t.user_id=$2) AND (t.message ILIKE $3 OR t.request->>'prompt' ILIKE $3) ORDER BY t.created_at DESC LIMIT $4 OFFSET $5")).bind(all).bind(auth.user.id).bind(query.search()).bind(limit).bind(offset).fetch_all(&app.pool).await?;
    Ok(Json(
        json!({"items":rows,"page":offset/limit+1,"per_page":limit,"has_more":rows.len()==limit as usize,"server_now":chrono::Utc::now()}),
    ))
}
pub async fn detail(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    authorize(&app, &auth, id).await?;
    let row =
        sqlx::query_scalar::<_, Value>(&format!("SELECT {TASK_VIEW} FROM tasks t WHERE id=$1"))
            .bind(id)
            .fetch_one(&app.pool)
            .await?;
    Ok(Json(json!({"task":row,"server_now":chrono::Utc::now()})))
}
pub async fn upstream(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    authorize(&app, &auth, id).await?;
    let rows = sqlx::query_scalar::<_, Value>(
        "SELECT to_jsonb(r) FROM upstream_requests r WHERE task_id=$1 ORDER BY started_at",
    )
    .bind(id)
    .fetch_all(&app.pool)
    .await?;
    Ok(Json(json!({"items":rows})))
}
pub async fn images(
    State(app): State<App>,
    auth: Auth,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    auth.user.require("task.read_own")?;
    let all = query.all_users.unwrap_or(false);
    if all {
        auth.user.admin()?;
        audit(&app, auth.user.id, "images", "asset.admin_list", json!({})).await?;
    }
    let (limit, offset) = query.paging();
    let rows=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(a)-'object_key'-'thumbnail_key'||jsonb_build_object('model_display_name',t.model_snapshot->>'display_name','entry_format_id',t.entry_snapshot->>'id','prompt',t.request->>'prompt') FROM assets a JOIN tasks t ON t.id=a.task_id WHERE a.kind='result' AND ($1 OR a.user_id=$2) AND t.request->>'prompt' ILIKE $3 ORDER BY a.created_at DESC LIMIT $4 OFFSET $5").bind(all).bind(auth.user.id).bind(query.search()).bind(limit).bind(offset).fetch_all(&app.pool).await?;
    Ok(Json(
        json!({"items":rows,"page":offset/limit+1,"per_page":limit,"has_more":rows.len()==limit as usize}),
    ))
}
pub async fn test_model(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    auth.user.admin()?;
    if body["confirm_cost"] != true {
        return Err(ApiError::validation(
            "真实测试可能计费，必须明确 confirm_cost=true",
        ));
    }
    body.as_object_mut().unwrap().remove("confirm_cost");
    body["model_config_id"] = json!(id);
    create_inner(&app, &auth, &headers, body).await
}
