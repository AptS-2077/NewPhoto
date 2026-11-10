use crate::{
    admin::ListQuery,
    auth::{Auth, audit, hash},
    error::{ApiError, ApiResult, reject_unknown, revision, text},
    net,
    registry::{Entry, Registry, validate_schema},
    state::App,
    storage,
};
use axum::{
    Json,
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use sqlx::Row;
use std::{
    collections::{HashMap, HashSet},
    io::{Cursor, Read},
    sync::Arc,
};
use uuid::Uuid;

const MAX_PACKAGE: usize = 10 * 1024 * 1024;
struct Package {
    manifest: Value,
    entries: Vec<Entry>,
    config_schema: Value,
    digest: String,
    bytes: Vec<u8>,
}
fn validate_pointer(pointer: &str) -> bool {
    (pointer.is_empty() || pointer.starts_with('/'))
        && !pointer
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0] == b'~' && !matches!(pair[1], b'0' | b'1'))
        && !pointer.ends_with('~')
}
fn unpack(bytes: Vec<u8>, approve_no_auth: bool) -> ApiResult<Package> {
    if bytes.len() > MAX_PACKAGE {
        return Err(ApiError::validation("插件包超过 10 MiB"));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(&bytes))
        .map_err(|_| ApiError::validation("无效 ZIP 插件包"))?;
    if archive.len() > 128 {
        return Err(ApiError::validation("压缩条目过多"));
    }
    let mut files = HashMap::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|_| ApiError::validation("无法读取压缩条目"))?;
        let name = file.name().to_string();
        if file.enclosed_name().is_none()
            || name.contains(['\\', ':'])
            || name.starts_with('/')
            || name.split('/').any(|part| part == ".." || part == ".")
            || file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(ApiError::validation("拒绝绝对路径、路径穿越和符号链接"));
        }
        total = total
            .checked_add(file.size())
            .ok_or_else(|| ApiError::validation("解压超限"))?;
        if total > 50 * 1024 * 1024 || file.size() > 4 * 1024 * 1024 {
            return Err(ApiError::validation("解压内容超过限制"));
        }
        if file.is_dir() {
            continue;
        }
        let mut content = Vec::new();
        (&mut file)
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut content)
            .map_err(|_| ApiError::validation("解压失败"))?;
        if content.len() > 4 * 1024 * 1024 || files.insert(name, content).is_some() {
            return Err(ApiError::validation("压缩条目过大或重复"));
        }
    }
    let parse = |name: &str| -> ApiResult<Value> {
        let content = files
            .get(name)
            .ok_or_else(|| ApiError::validation(format!("缺少 {name}")))?;
        serde_json::from_slice(content)
            .map_err(|_| ApiError::validation(format!("{name} 不是有效 JSON")))
    };
    let manifest = parse("manifest.json")?;
    if manifest["kind"] == "feature" {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "UNSUPPORTED_PLUGIN_KIND",
            "V1 不执行功能插件",
        ));
    }
    validate_schema(
        &serde_json::from_str(include_str!("../../schemas/manifest-v1.json")).unwrap(),
        &manifest,
    )?;
    let definition_schema: Value =
        serde_json::from_str(include_str!("../../schemas/entry-definition-v1.json")).unwrap();
    let mut entries = Vec::new();
    let mut ids = HashSet::new();
    let digest = hash(&bytes);
    for declaration in manifest["entrypoints"].as_array().unwrap() {
        let id = text(declaration, "entry_format_id")?;
        if id.starts_with("openai.") || id.starts_with("newphoto.") || !ids.insert(id.to_string()) {
            return Err(ApiError::validation("入口 ID 重复或使用内置保留前缀"));
        }
        let definition = parse(text(declaration, "definition")?)?;
        validate_schema(&definition_schema, &definition)?;
        net::join(
            "https://example.invalid/",
            text(&definition["request"], "path")?,
        )?;
        if definition["operation"] != declaration["operation"] {
            return Err(ApiError::validation("入口 operation 不一致"));
        }
        if definition["request"]["authentication"] == "none" && !approve_no_auth {
            return Err(ApiError::validation("无认证入口必须由管理员明确批准"));
        }
        if definition["operation"] == "edit"
            && (definition["request"]["encoding"] != "multipart"
                || definition["input_slots"] != json!(["image"])
                || definition["request"]["files"]
                    .as_array()
                    .is_none_or(|files| files.len() != 1))
        {
            return Err(ApiError::validation(
                "编辑入口必须声明 image 槽并使用 multipart 文件",
            ));
        }
        if definition["operation"] == "generate" && definition["input_slots"] != json!([]) {
            return Err(ApiError::validation("生成入口不能声明编辑原图"));
        }
        if let Some(files) = definition["request"]["files"].as_array() {
            if !files.is_empty()
                && (definition["request"]["encoding"] != "multipart"
                    || definition["input_slots"] != json!(["image"]))
            {
                return Err(ApiError::validation("文件槽未声明或编码不正确"));
            }
        }
        for pointer in [
            definition["response"]["error_pointer"].as_str(),
            definition["response"]["images_pointer"].as_str(),
        ]
        .into_iter()
        .flatten()
        .chain(
            definition["response"]["sources"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|source| source["pointer"].as_str()),
        ) {
            if !validate_pointer(pointer) {
                return Err(ApiError::validation("响应 Pointer 不符合 RFC 6901"));
            }
        }
        entries.push(Entry {
            id: id.into(),
            name: text(declaration, "name")?.into(),
            operation: text(declaration, "operation")?.into(),
            definition,
            installation_id: None,
            package_id: None,
            digest: digest.clone(),
            version: text(&manifest, "version")?.into(),
        });
    }
    let config_schema = if let Some(path) = manifest["config_schema"].as_str() {
        parse(path)?
    } else {
        json!({"type":"object","additionalProperties":false})
    };
    validate_schema(&config_schema, &json!({}))?;
    Ok(Package {
        manifest,
        entries,
        config_schema,
        digest,
        bytes,
    })
}
const PLUGIN_SELECT: &str = "SELECT to_jsonb(i)||jsonb_build_object('plugin_id',p.plugin_id,'version',p.version,'digest',p.digest,'manifest',p.manifest,'entries',p.entries,'config_schema',p.config_schema) FROM plugin_installations i JOIN plugin_packages p ON p.id=i.package_id";
pub async fn list(
    State(app): State<App>,
    auth: Auth,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    let (limit, offset) = query.paging();
    let rows = sqlx::query_scalar::<_, Value>(&format!(
        "{PLUGIN_SELECT} WHERE removed_at IS NULL AND (p.plugin_id ILIKE $1 OR i.alias ILIKE $1 OR p.manifest->>'name' ILIKE $1) ORDER BY i.created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(query.search()).bind(limit).bind(offset)
    .fetch_all(&app.pool)
    .await?;
    Ok(Json(
        json!({"items":rows,"page":offset/limit+1,"per_page":limit,"has_more":rows.len()==limit as usize,"registry_revision":app.registry.load().revision}),
    ))
}
pub async fn detail(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    let row = sqlx::query_scalar::<_, Value>(&format!(
        "{PLUGIN_SELECT} WHERE i.id=$1 AND removed_at IS NULL"
    ))
    .bind(id)
    .fetch_optional(&app.pool)
    .await?
    .ok_or_else(ApiError::missing)?;
    Ok(Json(row))
}
pub async fn install(
    State(app): State<App>,
    auth: Auth,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<Value>)> {
    auth.user.admin()?;
    let mut package = None;
    let mut approve = false;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::validation("上传格式无效"))?
    {
        if field.name() == Some("approve_no_auth") {
            approve = field
                .text()
                .await
                .map_err(|_| ApiError::validation("批准字段无效"))?
                == "true";
            continue;
        }
        if field.name() != Some("package") || package.is_some() {
            return Err(ApiError::validation("只允许一个 package 文件"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| ApiError::validation("上传读取失败"))?
        {
            if bytes.len() + chunk.len() > MAX_PACKAGE {
                return Err(ApiError::validation("插件包超过 10 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        package = Some(bytes);
    }
    let bytes = package.ok_or_else(|| ApiError::validation("缺少插件包"))?;
    let package = tokio::task::spawn_blocking(move || unpack(bytes, approve))
        .await
        .map_err(|_| ApiError::validation("插件校验失败"))??;
    let plugin_id = text(&package.manifest, "plugin_id")?;
    let version = text(&package.manifest, "version")?;
    let key = format!("plugins/{}.npplugin", package.digest);
    storage::atomic_write(&app, &key, &package.bytes).await?;
    let mut transaction = app.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(734203)")
        .execute(&mut *transaction)
        .await?;
    if let Some(existing)=sqlx::query("SELECT p.digest,i.id,i.removed_at FROM plugin_packages p JOIN plugin_installations i ON i.package_id=p.id WHERE p.plugin_id=$1 AND p.version=$2").bind(plugin_id).bind(version).fetch_optional(&mut *transaction).await?{
        if existing.get::<String,_>("digest")!=package.digest{return Err(ApiError::conflict("CONFLICT","相同 ID 与版本的包内容不同；请提高版本号"));}
        if existing.get::<Option<chrono::DateTime<chrono::Utc>>,_>("removed_at").is_some(){return Err(ApiError::conflict("CONFLICT","此版本已软移除；请安装新版本"));}
        return Ok((StatusCode::OK,Json(json!({"id":existing.get::<Uuid,_>("id"),"state":"existing","message":"相同内容已安装，未自动发布"}))));
    }
    let package_id = Uuid::new_v4();
    let installation_id = Uuid::new_v4();
    let entries: Vec<Entry> = package
        .entries
        .into_iter()
        .map(|mut entry| {
            entry.package_id = Some(package_id);
            entry.installation_id = Some(installation_id);
            entry
        })
        .collect();
    sqlx::query("INSERT INTO plugin_packages(id,plugin_id,version,digest,manifest,entries,config_schema,package_key) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(package_id).bind(plugin_id).bind(version).bind(&package.digest).bind(&package.manifest).bind(json!(entries)).bind(package.config_schema).bind(key).execute(&mut *transaction).await?;
    sqlx::query("INSERT INTO plugin_installations(id,package_id) VALUES($1,$2)")
        .bind(installation_id)
        .bind(package_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    audit(
        &app,
        auth.user.id,
        installation_id,
        "plugin.install",
        json!({"digest":package.digest}),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"id":installation_id,"revision":1,"state":"installed_pending","registry_revision":app.registry.load().revision,"message":"插件已安装，尚未生效。请选择立即刷新生效或稍后刷新。"}),
        ),
    ))
}
pub async fn update(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    reject_unknown(
        &body,
        &["alias", "notes", "config", "desired_enabled", "revision"],
    )?;
    let old=sqlx::query("SELECT i.*,p.config_schema FROM plugin_installations i JOIN plugin_packages p ON p.id=i.package_id WHERE i.id=$1 AND removed_at IS NULL").bind(id).fetch_optional(&app.pool).await?.ok_or_else(ApiError::missing)?;
    let config = body
        .get("config")
        .cloned()
        .unwrap_or_else(|| old.get("config"));
    validate_schema(&old.get("config_schema"), &config)?;
    let alias = body["alias"].as_str().unwrap_or(old.get("alias"));
    let notes = body["notes"].as_str().unwrap_or(old.get("notes"));
    if alias.len() > 128 || notes.len() > 4096 {
        return Err(ApiError::validation("别名或备注过长"));
    }
    let changed=sqlx::query("UPDATE plugin_installations SET alias=$1,notes=$2,config=$3,desired_enabled=$4,state='installed_pending',revision=revision+1 WHERE id=$5 AND revision=$6 AND removed_at IS NULL").bind(alias).bind(notes).bind(config).bind(body["desired_enabled"].as_bool().unwrap_or(old.get("desired_enabled"))).bind(id).bind(revision(&body)?).execute(&app.pool).await?.rows_affected();
    if changed == 0 {
        return Err(ApiError::conflict("CONFLICT", "插件配置版本已改变"));
    }
    audit(&app, auth.user.id, id, "plugin.configure", json!({})).await?;
    Ok(Json(
        json!({"ok":true,"message":"已保存待刷新；当前生效状态未改变"}),
    ))
}
pub async fn refresh(
    State(app): State<App>,
    auth: Auth,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    reject_unknown(
        &body,
        &["expected_registry_revision", "request_id", "changes"],
    )?;
    let request_id = text(&body, "request_id")?;
    if request_id.len() > 128 {
        return Err(ApiError::validation("request_id 过长"));
    }
    let request_hash = hash(serde_json::to_vec(&body).unwrap());
    let _guard = app.registry_lock.lock().await;
    let mut transaction = app.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(734204)")
        .execute(&mut *transaction)
        .await?;
    if let Some(row) =
        sqlx::query("SELECT revision,request_hash FROM registry_revisions WHERE request_id=$1")
            .bind(request_id)
            .fetch_optional(&mut *transaction)
            .await?
    {
        if row.get::<String, _>("request_hash") != request_hash {
            return Err(ApiError::conflict(
                "CONFLICT",
                "request_id 已用于其他发布内容",
            ));
        }
        return Ok(Json(
            json!({"registry_revision":row.get::<i64,_>("revision"),"replayed":true}),
        ));
    }
    let snapshot = app.registry.load_full();
    if body["expected_registry_revision"].as_i64() != Some(snapshot.revision) {
        return Err(ApiError::conflict(
            "REGISTRY_CHANGED",
            "入口注册表已改变，请刷新后确认发布清单",
        ));
    }
    let changes = body["changes"]
        .as_array()
        .filter(|changes| !changes.is_empty() && changes.len() <= 32)
        .ok_or_else(|| ApiError::validation("必须明确选择 1–32 项待发布变更"))?;
    let mut selected = HashSet::new();
    let mut entries = snapshot.entries.clone();
    let mut changed = false;
    for change in changes {
        reject_unknown(change, &["id", "revision"])?;
        let id: Uuid = text(change, "id")?
            .parse()
            .map_err(|_| ApiError::validation("插件 ID 无效"))?;
        if !selected.insert(id) {
            return Err(ApiError::validation("发布清单包含重复项"));
        }
        let row=sqlx::query("SELECT i.*,p.entries,p.plugin_id,p.config_schema FROM plugin_installations i JOIN plugin_packages p ON p.id=i.package_id WHERE i.id=$1 AND removed_at IS NULL FOR UPDATE OF i").bind(id).fetch_optional(&mut *transaction).await?.ok_or_else(ApiError::missing)?;
        if revision(change)? != row.get::<i64, _>("revision") {
            return Err(ApiError::conflict("CONFLICT", "待发布配置已改变"));
        }
        if row.get::<String, _>("state") == "published" {
            continue;
        }
        changed = true;
        let enabled: bool = row.get("desired_enabled");
        let package_id: Uuid = row.get("package_id");
        validate_schema(&row.get("config_schema"), &row.get("config"))?;
        let candidate: Vec<Entry> = serde_json::from_value(row.get("entries"))
            .map_err(|_| ApiError::validation("包入口记录损坏"))?;
        let plugin_id: String = row.get("plugin_id");
        let old_installations:Vec<Uuid>=sqlx::query_scalar("SELECT i.id FROM plugin_installations i JOIN plugin_packages p ON p.id=i.package_id WHERE p.plugin_id=$1").bind(&plugin_id).fetch_all(&mut *transaction).await?;
        entries.retain(|entry| {
            !entry
                .installation_id
                .is_some_and(|installation| old_installations.contains(&installation))
        });
        if enabled {
            entries.extend(candidate);
        }
        sqlx::query("UPDATE plugin_installations SET active_enabled=false,state=CASE WHEN id=$1 THEN 'published' ELSE state END WHERE id=ANY($2)").bind(id).bind(&old_installations).execute(&mut *transaction).await?;
        sqlx::query("UPDATE plugin_installations SET active_enabled=$1,state='published',published_revision=$2 WHERE id=$3").bind(enabled).bind(snapshot.revision+1).bind(id).execute(&mut *transaction).await?;
        let _ = package_id;
    }
    if !changed {
        return Ok(Json(
            json!({"registry_revision":snapshot.revision,"no_change":true}),
        ));
    }
    let mut ids = HashSet::new();
    for entry in &entries {
        if !ids.insert(&entry.id) {
            return Err(ApiError::validation("入口 ID 冲突，原快照保持不变"));
        }
    }
    let active_ids: Vec<String> = entries.iter().map(|entry| entry.id.clone()).collect();
    let references:i64=sqlx::query_scalar("SELECT count(*) FROM model_configs WHERE enabled AND deleted_at IS NULL AND NOT(entry_format_id=ANY($1))").bind(&active_ids).fetch_one(&mut *transaction).await?;
    if references > 0 {
        return Err(ApiError::conflict(
            "PLUGIN_IN_USE",
            format!("{references} 个启用模型引用即将移除的入口，请先停用模型"),
        ));
    }
    let next = Registry {
        revision: snapshot.revision + 1,
        entries,
    };
    sqlx::query("INSERT INTO registry_revisions(revision,snapshot,request_id,request_hash) VALUES($1,$2,$3,$4)").bind(next.revision).bind(json!(next)).bind(request_id).bind(request_hash).execute(&mut *transaction).await?;
    transaction.commit().await?;
    app.registry.store(Arc::new(next.clone()));
    audit(
        &app,
        auth.user.id,
        next.revision,
        "registry.publish",
        json!({"selected":selected}),
    )
    .await?;
    Ok(Json(
        json!({"registry_revision":next.revision,"message":"指定变更已原子发布；在途任务继续使用原快照"}),
    ))
}
pub async fn remove(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    auth.user.admin()?;
    let _guard = app.registry_lock.lock().await;
    let row=sqlx::query("SELECT i.*,p.entries FROM plugin_installations i JOIN plugin_packages p ON p.id=i.package_id WHERE i.id=$1 AND removed_at IS NULL").bind(id).fetch_optional(&app.pool).await?.ok_or_else(ApiError::missing)?;
    let package_id: Uuid = row.get("package_id");
    let entries: Vec<Entry> = serde_json::from_value(row.get("entries")).unwrap_or_default();
    let ids: Vec<String> = entries.iter().map(|entry| entry.id.clone()).collect();
    let models: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM model_configs WHERE entry_format_id=ANY($1) AND deleted_at IS NULL",
    )
    .bind(&ids)
    .fetch_one(&app.pool)
    .await?;
    let tasks:i64=sqlx::query_scalar("SELECT count(*) FROM tasks WHERE entry_snapshot->>'package_id'=$1 AND status NOT IN ('succeeded','failed','result_unavailable')").bind(package_id.to_string()).fetch_one(&app.pool).await?;
    if row.get::<bool, _>("active_enabled") || models > 0 || tasks > 0 {
        return Err(ApiError::conflict(
            "PLUGIN_IN_USE",
            format!(
                "请先停用并刷新，处理 {models} 个模型引用和 {tasks} 个在途/待核对任务；模型：/admin/models，任务：/tasks"
            ),
        ));
    }
    let changed=sqlx::query("UPDATE plugin_installations SET removed_at=now(),revision=revision+1 WHERE id=$1 AND revision=$2").bind(id).bind(revision(&body)?).execute(&app.pool).await?.rows_affected();
    if changed == 0 {
        return Err(ApiError::conflict("CONFLICT", "插件版本已改变"));
    }
    audit(&app, auth.user.id, id, "plugin.remove", json!({})).await?;
    Ok(Json(json!({"ok":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointers() {
        assert!(validate_pointer("/a~1b/~0"));
        assert!(!validate_pointer("/a~2"));
        assert!(!validate_pointer("a"));
    }
    #[test]
    fn builtins_match_schema() {
        let schema =
            serde_json::from_str(include_str!("../../schemas/entry-definition-v1.json")).unwrap();
        for entry in Registry::initial().entries {
            validate_schema(&schema, &entry.definition).unwrap();
        }
    }
}
