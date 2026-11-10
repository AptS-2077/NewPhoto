use crate::{
    auth::user_by_id,
    error::{ApiError, ApiResult},
    net,
    registry::Entry,
    state::App,
    storage,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Map, Value, json};
use sqlx::Row;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use url::Url;
use uuid::Uuid;

pub async fn recover(app: &App) -> anyhow::Result<()> {
    sqlx::query("UPDATE tasks t SET status=CASE WHEN EXISTS(SELECT 1 FROM assets WHERE task_id=t.id AND kind='result') THEN 'succeeded' WHEN a.response_complete AND a.receipt_key IS NOT NULL THEN 'saving' WHEN a.request_may_have_been_sent THEN 'submission_unknown' ELSE 'queued' END,message=CASE WHEN a.request_may_have_been_sent AND NOT a.response_complete THEN '进程中断；结果待确认，上游可能已执行，禁止自动重新提交' ELSE '服务恢复，继续核对已持久化成果' END,lease_until=NULL,lease_token=NULL,ended_at=CASE WHEN a.request_may_have_been_sent AND NOT a.response_complete THEN now() ELSE t.ended_at END FROM task_attempts a WHERE a.task_id=t.id AND t.status IN ('preparing','submitting','saving')").execute(&app.pool).await?;
    sqlx::query("UPDATE tasks SET status='queued',lease_until=NULL,lease_token=NULL WHERE status='preparing' AND NOT EXISTS(SELECT 1 FROM task_attempts WHERE task_id=tasks.id)").execute(&app.pool).await?;
    sqlx::query("UPDATE tasks SET total_ms=GREATEST(0,(extract(epoch FROM (ended_at-created_at))*1000)::bigint),execution_ms=GREATEST(0,(extract(epoch FROM (ended_at-started_at))*1000)::bigint) WHERE ended_at IS NOT NULL AND total_ms IS NULL").execute(&app.pool).await?;
    sqlx::query(
        "UPDATE tasks SET error_code='SUBMISSION_UNKNOWN' WHERE status='submission_unknown'",
    )
    .execute(&app.pool)
    .await?;
    Ok(())
}
pub async fn run(app: App) {
    let slots = Arc::new(Semaphore::new(app.config.concurrency));
    loop {
        if let Ok(permit) = slots.clone().try_acquire_owned() {
            let lease = Uuid::new_v4();
            let selected=sqlx::query("UPDATE tasks SET status=CASE WHEN status='saving' THEN 'saving' ELSE 'preparing' END,started_at=COALESCE(started_at,now()),queue_ms=COALESCE(queue_ms,GREATEST(0,(extract(epoch FROM(now()-created_at))*1000)::bigint)),lease_until=now()+interval '120 seconds',lease_token=$1 WHERE id=(SELECT id FROM tasks WHERE (status='saving' OR (status='queued' AND (SELECT value FROM app_settings WHERE key='generation_paused')='false'::jsonb)) AND (lease_until IS NULL OR lease_until<now()) ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING id").bind(lease).fetch_optional(&app.pool).await;
            match selected {
                Ok(Some(row)) => {
                    let app = app.clone();
                    let id = row.get("id");
                    tokio::spawn(async move {
                        let _permit = permit;
                        execute(app, id, lease).await;
                    });
                }
                Ok(None) => drop(permit),
                Err(error) => {
                    drop(permit);
                    tracing::error!(error=%error,"task claim failed");
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
}
async fn execute(app: App, id: Uuid, lease: Uuid) {
    let heartbeat_app = app.clone();
    let heartbeat = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            let _=sqlx::query("UPDATE tasks SET lease_until=now()+interval '120 seconds' WHERE id=$1 AND lease_token=$2").bind(id).bind(lease).execute(&heartbeat_app.pool).await;
        }
    });
    let started = Instant::now();
    let result = process(&app, id).await;
    if let Err(error) = result {
        if error.code == "GENERATION_PAUSED" {
            heartbeat.abort();
            return;
        }
        let status = sqlx::query_scalar::<_, String>("SELECT status FROM tasks WHERE id=$1")
            .bind(id)
            .fetch_one(&app.pool)
            .await
            .unwrap_or_default();
        if !matches!(
            status.as_str(),
            "submission_unknown" | "failed" | "result_unavailable" | "succeeded"
        ) {
            if status == "saving" && matches!(error.code, "STORAGE_ERROR" | "INTERNAL_ERROR") {
                let retries=sqlx::query_scalar::<_,i32>("UPDATE tasks SET retry_count=retry_count+1,lease_until=now()+interval '5 seconds',lease_token=NULL,message='成果保存失败；仅重试解析和入库，不重新生成' WHERE id=$1 RETURNING retry_count").bind(id).fetch_one(&app.pool).await.unwrap_or(3);
                if retries >= 3 {
                    let _ = finish(
                        &app,
                        id,
                        "result_unavailable",
                        "成果入库失败，保留回执和上游证据，不会重新生成",
                        started,
                    )
                    .await;
                }
            } else {
                let terminal = if status == "saving" {
                    "result_unavailable"
                } else {
                    "failed"
                };
                let _ = finish(&app, id, terminal, &error.message, started).await;
            }
        }
        sqlx::query("UPDATE tasks SET error_code=CASE status WHEN 'submission_unknown' THEN 'SUBMISSION_UNKNOWN' WHEN 'result_unavailable' THEN 'RESULT_UNAVAILABLE' ELSE $1 END WHERE id=$2 AND status IN ('failed','submission_unknown','result_unavailable')").bind(error.code).bind(id).execute(&app.pool).await.ok();
    } else {
        let _ = finish(
            &app,
            id,
            "succeeded",
            "图片已保存，可授权预览和下载",
            started,
        )
        .await;
    }
    heartbeat.abort();
}
async fn finish(
    app: &App,
    id: Uuid,
    status: &str,
    message: &str,
    _started: Instant,
) -> ApiResult<()> {
    sqlx::query("UPDATE tasks SET status=$1,message=$2,ended_at=now(),execution_ms=GREATEST(0,(extract(epoch FROM(now()-started_at))*1000)::bigint),total_ms=GREATEST(0,(extract(epoch FROM(now()-created_at))*1000)::bigint),error_code=CASE $1 WHEN 'submission_unknown' THEN 'SUBMISSION_UNKNOWN' WHEN 'result_unavailable' THEN 'RESULT_UNAVAILABLE' ELSE NULL END,lease_until=NULL,lease_token=NULL WHERE id=$3").bind(status).bind(message).bind(id).execute(&app.pool).await?;
    Ok(())
}
async fn stage(app: &App, id: Uuid, status: &str, message: &str) -> ApiResult<()> {
    sqlx::query("UPDATE tasks SET status=$1,message=$2 WHERE id=$3")
        .bind(status)
        .bind(message)
        .bind(id)
        .execute(&app.pool)
        .await?;
    Ok(())
}
async fn record(app: &App, task: Uuid, phase: &str) -> ApiResult<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO upstream_requests(id,task_id,phase) VALUES($1,$2,$3)")
        .bind(id)
        .bind(task)
        .bind(phase)
        .execute(&app.pool)
        .await?;
    Ok(id)
}
async fn transport(
    app: &App,
    id: Uuid,
    kind: &str,
    summary: &str,
    started: Instant,
) -> ApiResult<()> {
    sqlx::query("UPDATE upstream_requests SET ended_at=now(),duration_ms=$1,transport_error=$2,summary=$3 WHERE id=$4").bind(started.elapsed().as_millis() as i64).bind(kind).bind(summary).bind(id).execute(&app.pool).await?;
    Ok(())
}
struct Receipt {
    bytes: Vec<u8>,
    content_type: String,
    status: u16,
}
async fn capture(
    app: &App,
    id: Uuid,
    response: reqwest::Response,
    limit: usize,
    started: Instant,
) -> ApiResult<Receipt> {
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|header| header.to_str().ok())
        .unwrap_or("")
        .to_string();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|header| header.to_str().ok())
        .filter(|value| {
            value.len() <= 256
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
        .map(str::to_string);
    sqlx::query("UPDATE upstream_requests SET headers_at=now(),http_status=$1,summary=$2,upstream_request_id=$3 WHERE id=$4").bind(i32::from(status)).bind(format!("HTTP {status}；正在读取响应体")).bind(request_id).bind(id).execute(&app.pool).await?;
    let mut response = response;
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len() + chunk.len() > limit {
                    transport(app, id, "body_limit", "响应体超出平台上限", started).await?;
                    return Err(ApiError::validation("响应体超过平台上限"));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => {
                transport(
                    app,
                    id,
                    "body_read",
                    "响应头已收到，但响应体读取失败",
                    started,
                )
                .await?;
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "BODY_READ_FAILED",
                    format!("HTTP {status}；响应体读取失败"),
                ));
            }
        }
    }
    sqlx::query(
        "UPDATE upstream_requests SET ended_at=now(),duration_ms=$1,summary=$2 WHERE id=$3",
    )
    .bind(started.elapsed().as_millis() as i64)
    .bind(format!("HTTP {status}；响应体接收完成"))
    .bind(id)
    .execute(&app.pool)
    .await?;
    Ok(Receipt {
        bytes,
        content_type,
        status,
    })
}
use axum::http::StatusCode;
async fn process(app: &App, id: Uuid) -> ApiResult<()> {
    let task = sqlx::query("SELECT * FROM tasks WHERE id=$1")
        .bind(id)
        .fetch_one(&app.pool)
        .await?;
    let owner: Uuid = task.get("user_id");
    let existing: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assets WHERE task_id=$1 AND kind='result')",
    )
    .bind(id)
    .fetch_one(&app.pool)
    .await?;
    if existing {
        return Ok(());
    }
    let entry: Entry = serde_json::from_value(task.get("entry_snapshot"))
        .map_err(|_| ApiError::validation("任务入口快照损坏"))?;
    let model: Value = task.get("model_snapshot");
    let request: Value = task.get("request");
    if let Some(receipt)=sqlx::query("SELECT receipt_key,response_content_type,submission_status FROM task_attempts WHERE task_id=$1 AND response_complete").bind(id).fetch_optional(&app.pool).await?{
        let key:String=receipt.get("receipt_key");let content_type:String=receipt.get("response_content_type");
        stage(app,id,"saving","从持久回执恢复成果，不重新调用生成接口").await?;
        return save_receipt(app,id,owner,&entry,&model,tokio::fs::read(app.config.data_dir.join(key)).await?,&content_type).await;
    }
    let base = text_url(&model)?;
    let target = net::join(
        base,
        entry.definition["request"]["path"].as_str().unwrap_or(""),
    )?;
    let started = Instant::now();
    let client = match net::client(&app.config, &target, None).await {
        Ok(client) => client,
        Err(error) => {
            let record_id = record(app, id, "submit").await?;
            transport(app, record_id, "dns_or_policy", &error.message, started).await?;
            return Err(error);
        }
    };
    let context = json!({"request":request,"model":{"upstream_model":model["upstream_model"]}});
    let fields = entry.definition["request"]["fields"]
        .as_object()
        .ok_or_else(|| ApiError::validation("请求映射无效"))?;
    let mut mapped = Map::new();
    for (name, mapping) in fields {
        let value = if let Some(pointer) = mapping["from"].as_str() {
            context
                .pointer(pointer)
                .filter(|value| !value.is_null())
                .cloned()
                .ok_or_else(|| ApiError::validation("映射缺少必填来源"))?
        } else {
            mapping["constant"].clone()
        };
        mapped.insert(name.clone(), value);
    }
    let mut builder = client.post(target);
    if entry.definition["request"]["encoding"] == "json" {
        builder = builder.json(&mapped);
    } else {
        let mut form = reqwest::multipart::Form::new();
        for (name, value) in mapped {
            form = form.text(
                name,
                if let Some(text) = value.as_str() {
                    text.to_string()
                } else {
                    value.to_string()
                },
            );
        }
        if let Some(files) = entry.definition["request"]["files"].as_array() {
            for file in files {
                let asset_id: Uuid = request["input_asset_ids"][0]
                    .as_str()
                    .ok_or_else(|| ApiError::validation("缺少原图"))?
                    .parse()
                    .map_err(|_| ApiError::validation("原图 ID 错误"))?;
                let asset=sqlx::query("SELECT object_key,mime FROM assets WHERE id=$1 AND user_id=$2 AND kind='input'").bind(asset_id).bind(owner).fetch_optional(&app.pool).await?.ok_or_else(ApiError::missing)?;
                let bytes = tokio::fs::read(
                    app.config
                        .data_dir
                        .join(asset.get::<String, _>("object_key")),
                )
                .await?;
                let mime: String = asset.get("mime");
                let extension = match mime.as_str() {
                    "image/jpeg" => "jpg",
                    "image/webp" => "webp",
                    _ => "png",
                };
                let part = reqwest::multipart::Part::bytes(bytes)
                    .file_name(format!("input.{extension}"))
                    .mime_str(&mime)
                    .map_err(|_| ApiError::validation("原图格式无效"))?;
                form = form.part(file["field"].as_str().unwrap_or("image").to_string(), part);
            }
        }
        builder = builder.multipart(form);
    }
    let user = user_by_id(&app.pool, owner).await?;
    user.require("image.generate")?;
    user.require("task.read_own")?;
    let model_id: Uuid = task.get("model_id");
    if !user.model(model_id) {
        return Err(ApiError::forbidden());
    }
    let current=sqlx::query("SELECT credential_cipher,credential_epoch FROM model_configs WHERE id=$1 AND enabled AND deleted_at IS NULL").bind(model_id).fetch_optional(&app.pool).await?.ok_or_else(||ApiError::validation("模型停用或删除，排队任务已终止"))?;
    if current.get::<i64, _>("credential_epoch") != model["credential_epoch"].as_i64().unwrap_or(0)
    {
        return Err(ApiError::validation("凭据已变更或撤销，排队任务已终止"));
    }
    if entry.definition["request"]["authentication"] == "deployment_bearer" {
        let cipher: Option<String> = current.get("credential_cipher");
        let secret = app
            .config
            .decrypt(&cipher.ok_or_else(|| ApiError::validation("模型没有凭据"))?)
            .map_err(|_| ApiError::validation("模型凭据不可用"))?;
        builder = builder.bearer_auth(secret);
    }
    let paused: Value =
        sqlx::query_scalar("SELECT value FROM app_settings WHERE key='generation_paused'")
            .fetch_one(&app.pool)
            .await?;
    if paused == true {
        sqlx::query("UPDATE tasks SET status='queued',message='恢复核对模式暂停外部生成',lease_until=NULL,lease_token=NULL WHERE id=$1").bind(id).execute(&app.pool).await?;
        return Err(ApiError::conflict("GENERATION_PAUSED", "外部生成已暂停"));
    }
    let record_id = record(app, id, "submit").await?;
    let started = Instant::now();
    sqlx::query("INSERT INTO task_attempts(id,task_id,request_may_have_been_sent) VALUES($1,$2,true) ON CONFLICT(task_id) DO UPDATE SET request_may_have_been_sent=true").bind(Uuid::new_v4()).bind(id).execute(&app.pool).await?;
    stage(app, id, "submitting", "等待上游响应；尚无 HTTP 状态").await?;
    let response = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            let kind = if error.is_timeout() {
                "timeout"
            } else if error.is_connect() {
                "connect_dns_tls"
            } else {
                "transport"
            };
            transport(app, record_id, kind, "无 HTTP 响应；传输失败", started).await?;
            let status = if error.is_connect() {
                "failed"
            } else {
                "submission_unknown"
            };
            finish(
                app,
                id,
                status,
                if status == "failed" {
                    "无 HTTP 响应；DNS、TLS 或连接失败"
                } else {
                    "结果待确认，上游可能已执行；不会自动重提"
                },
                started,
            )
            .await?;
            return Err(ApiError::validation("上游传输失败"));
        }
    };
    let response_status = response.status().as_u16();
    sqlx::query("UPDATE task_attempts SET submission_status=$1 WHERE task_id=$2")
        .bind(i32::from(response_status))
        .bind(id)
        .execute(&app.pool)
        .await?;
    let receipt = match capture(app, record_id, response, 40 * 1024 * 1024, started).await {
        Ok(receipt) => receipt,
        Err(error) => {
            let status = if (200..300).contains(&response_status) {
                "submission_unknown"
            } else {
                "failed"
            };
            finish(app, id, status, &error.message, started).await?;
            return Err(error);
        }
    };
    if !entry.definition["response"]["success_http_statuses"]
        .as_array()
        .is_some_and(|statuses| statuses.contains(&json!(receipt.status)))
    {
        finish(
            app,
            id,
            "failed",
            &format!(
                "上游 HTTP {}；{}",
                receipt.status,
                match receipt.status {
                    404 => "上游接口或资源未找到",
                    500..=599 => "上游服务错误",
                    _ => "请求失败",
                }
            ),
            started,
        )
        .await?;
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "UPSTREAM_HTTP_ERROR",
            "上游请求失败",
        ));
    }
    let receipt_key = format!("receipts/{id}.body");
    if let Err(error) = storage::atomic_write(app, &receipt_key, &receipt.bytes).await {
        finish(
            app,
            id,
            "result_unavailable",
            "上游已返回，但回执无法持久化；禁止重新生成",
            started,
        )
        .await?;
        return Err(error);
    }
    sqlx::query("UPDATE task_attempts SET response_complete=true,receipt_key=$1,response_content_type=$2 WHERE task_id=$3").bind(receipt_key).bind(&receipt.content_type).bind(id).execute(&app.pool).await?;
    stage(app, id, "saving", "上游已响应，正在解析和保存真实图片").await?;
    save_receipt(
        app,
        id,
        owner,
        &entry,
        &model,
        receipt.bytes,
        &receipt.content_type,
    )
    .await
}
fn text_url(model: &Value) -> ApiResult<&str> {
    model["api_base_url"]
        .as_str()
        .ok_or_else(|| ApiError::validation("模型地址快照损坏"))
}
async fn save_receipt(
    app: &App,
    id: Uuid,
    owner: Uuid,
    entry: &Entry,
    model: &Value,
    bytes: Vec<u8>,
    content_type: &str,
) -> ApiResult<()> {
    let response = &entry.definition["response"];
    let image = if response["encoding"] == "binary" {
        if !content_type.starts_with("image/") {
            return Err(ApiError::validation("二进制回执不是图片 Content-Type"));
        }
        bytes
    } else {
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::validation("上游 HTTP 成功但响应不是有效 JSON"))?;
        if response["error_pointer"]
            .as_str()
            .and_then(|pointer| value.pointer(pointer))
            .is_some_and(|error| {
                !error.is_null()
                    && error != &json!("")
                    && error != &json!({})
                    && error != &json!([])
                    && error != &json!(false)
            })
        {
            return Err(ApiError::validation("上游响应包含错误，未产生可用图片"));
        }
        let images = value
            .pointer(response["images_pointer"].as_str().unwrap_or(""))
            .and_then(Value::as_array)
            .filter(|images| images.len() == 1)
            .ok_or_else(|| ApiError::validation("上游必须返回恰好一个图片结果"))?;
        let sources: Vec<(&str, &str)> = response["sources"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|source| {
                let text = images[0].pointer(source["pointer"].as_str()?)?.as_str()?;
                if text.is_empty() {
                    None
                } else {
                    Some((source["kind"].as_str()?, text))
                }
            })
            .collect();
        if sources.len() != 1 {
            return Err(ApiError::validation("图片来源为空或 URL/Base64 来源冲突"));
        }
        match sources[0] {
            ("base64", encoded) => {
                if encoded.len() > ((storage::MAX_IMAGE + 2) / 3) * 4 {
                    return Err(ApiError::validation("Base64 图片超限"));
                }
                STANDARD
                    .decode(encoded)
                    .map_err(|_| ApiError::validation("Base64 图片解码失败"))?
            }
            ("url", url) => {
                download(
                    app,
                    id,
                    url,
                    &Url::parse(text_url(model)?)
                        .map_err(|_| ApiError::validation("模型地址无效"))?,
                )
                .await?
            }
            _ => return Err(ApiError::validation("不支持的图片来源")),
        }
    };
    storage::save(app, owner, Some(id), "result", image).await?;
    Ok(())
}
async fn download(app: &App, task: Uuid, value: &str, origin: &Url) -> ApiResult<Vec<u8>> {
    let mut url = Url::parse(value).map_err(|_| ApiError::validation("图片 URL 无效"))?;
    for _ in 0..4 {
        let id = record(app, task, "result_download").await?;
        let started = Instant::now();
        let client = match net::client(&app.config, &url, Some(origin)).await {
            Ok(client) => client,
            Err(error) => {
                transport(app, id, "dns_or_policy", &error.message, started).await?;
                return Err(error);
            }
        };
        let response = match client.get(url.clone()).send().await {
            Ok(response) => response,
            Err(_) => {
                transport(
                    app,
                    id,
                    "download_transport",
                    "成果下载无 HTTP 响应",
                    started,
                )
                .await?;
                return Err(ApiError::validation("成果下载传输失败；不重新生成"));
            }
        };
        let redirect = response.status().is_redirection();
        let location = response
            .headers()
            .get("location")
            .and_then(|header| header.to_str().ok())
            .map(str::to_string);
        let receipt = capture(app, id, response, storage::MAX_IMAGE, started).await?;
        if redirect {
            url = url
                .join(&location.ok_or_else(|| ApiError::validation("下载重定向缺少位置"))?)
                .map_err(|_| ApiError::validation("下载重定向位置无效"))?;
            continue;
        }
        if receipt.status != 200 {
            return Err(ApiError::validation(format!(
                "成果下载 HTTP {}；保留生成请求证据，不重新生成",
                receipt.status
            )));
        }
        return Ok(receipt.bytes);
    }
    Err(ApiError::validation("成果下载重定向过多"))
}
