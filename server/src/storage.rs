use crate::{
    auth::{Auth, audit, hash},
    error::{ApiError, ApiResult},
    state::App,
};
use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use image::{GenericImageView, ImageFormat, ImageReader};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use std::io::Cursor;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

pub const MAX_IMAGE: usize = 25 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 40_000_000;

pub async fn cleanup_unreferenced(app: &App) -> ApiResult<()> {
    for directory in ["images", "thumbnails", "receipts", "plugins"] {
        let mut entries = tokio::fs::read_dir(app.config.data_dir.join(directory)).await?;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_file() {
                continue;
            }
            let metadata = entry.metadata().await?;
            let age = metadata.modified()?.elapsed().unwrap_or_default();
            if age < std::time::Duration::from_secs(86400) {
                continue;
            }
            let filename = entry.file_name().to_string_lossy().into_owned();
            let key = format!("{directory}/{filename}");
            let temporary = filename.ends_with(".tmp");
            let orphan = if matches!(directory, "images" | "thumbnails") && !temporary {
                !sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM assets WHERE object_key=$1 OR thumbnail_key=$1)",
                )
                .bind(&key)
                .fetch_one(&app.pool)
                .await?
            } else {
                false
            };
            if temporary || orphan {
                tokio::fs::remove_file(entry.path()).await?;
            }
        }
    }
    Ok(())
}
pub async fn atomic_write(app: &App, key: &str, bytes: &[u8]) -> ApiResult<()> {
    let path = app.config.data_dir.join(key);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .await?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(temporary, path).await?;
    Ok(())
}
pub async fn save(
    app: &App,
    user_id: Uuid,
    task_id: Option<Uuid>,
    kind: &str,
    bytes: Vec<u8>,
) -> ApiResult<Value> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE {
        return Err(ApiError::validation("图片必须介于 1 字节和 25 MiB 之间"));
    }
    let permit = app
        .image_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError::validation("图片处理已关闭"))?;
    let (bytes, thumbnail, mime, width, height) =
        tokio::task::spawn_blocking(move || -> ApiResult<_> {
            let _permit = permit;
            let format = image::guess_format(&bytes)
                .map_err(|_| ApiError::validation("文件头不是有效图片"))?;
            let mime = match format {
                ImageFormat::Png => "image/png",
                ImageFormat::Jpeg => "image/jpeg",
                ImageFormat::WebP => "image/webp",
                _ => return Err(ApiError::validation("仅支持 PNG/JPEG/WebP")),
            };
            let (width, height) = ImageReader::with_format(Cursor::new(&bytes), format)
                .into_dimensions()
                .map_err(|_| ApiError::validation("无法读取图片尺寸"))?;
            if u64::from(width) * u64::from(height) > MAX_PIXELS || width == 0 || height == 0 {
                return Err(ApiError::validation("图片像素超过 40 MP 或尺寸无效"));
            }
            let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(256 * 1024 * 1024);
            reader.limits(limits);
            let decoded = reader
                .decode()
                .map_err(|_| ApiError::validation("图片无法解码或解码资源超限"))?;
            let mut thumbnail = Cursor::new(Vec::new());
            decoded
                .thumbnail(360, 360)
                .write_to(&mut thumbnail, ImageFormat::Png)
                .map_err(|_| ApiError::validation("缩略图编码失败"))?;
            let (width, height) = decoded.dimensions();
            Ok((bytes, thumbnail.into_inner(), mime, width, height))
        })
        .await
        .map_err(|_| ApiError::validation("图片处理失败"))??;
    let id = Uuid::new_v4();
    let key = format!("images/{id}.bin");
    let thumbnail_key = format!("thumbnails/{id}.png");
    atomic_write(app, &key, &bytes).await?;
    atomic_write(app, &thumbnail_key, &thumbnail).await?;
    sqlx::query("INSERT INTO assets(id,user_id,task_id,kind,object_key,thumbnail_key,digest,mime,width,height,byte_size) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)").bind(id).bind(user_id).bind(task_id).bind(kind).bind(&key).bind(&thumbnail_key).bind(hash(&bytes)).bind(mime).bind(width as i32).bind(height as i32).bind(bytes.len() as i64).execute(&app.pool).await?;
    Ok(
        json!({"id":id,"mime":mime,"width":width,"height":height,"byte_size":bytes.len(),"preview_url":format!("/api/v1/assets/{id}/preview"),"download_url":format!("/api/v1/assets/{id}/download")}),
    )
}
pub async fn upload(
    State(app): State<App>,
    auth: Auth,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<Value>)> {
    auth.user.require("image.generate")?;
    let mut image = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::validation("上传内容无效或超限"))?
    {
        if field.name() != Some("image") || image.is_some() {
            return Err(ApiError::validation("必须只上传一张 image"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| ApiError::validation("上传读取失败"))?
        {
            if bytes.len() + chunk.len() > MAX_IMAGE {
                return Err(ApiError::validation("图片超过 25 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        image = Some(bytes);
    }
    let asset = save(
        &app,
        auth.user.id,
        None,
        "input",
        image.ok_or_else(|| ApiError::validation("缺少原图"))?,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(asset)))
}
#[derive(Default, Deserialize)]
pub struct PreviewQuery {
    pub thumb: Option<bool>,
}
async fn read(
    app: &App,
    auth: Auth,
    id: Uuid,
    download: bool,
    thumbnail: bool,
) -> ApiResult<Response> {
    let row = sqlx::query("SELECT * FROM assets WHERE id=$1")
        .bind(id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(ApiError::missing)?;
    let owner: Uuid = row.get("user_id");
    if owner != auth.user.id && auth.user.role != "admin" {
        return Err(ApiError::missing());
    }
    auth.user.require("task.read_own")?;
    if download {
        auth.user.require("asset.download_own")?;
    }
    if owner != auth.user.id {
        audit(
            app,
            auth.user.id,
            id,
            "asset.admin_read",
            json!({"download":download}),
        )
        .await?;
    }
    let key: String = row.get(if thumbnail {
        "thumbnail_key"
    } else {
        "object_key"
    });
    let bytes = tokio::fs::read(app.config.data_dir.join(key)).await?;
    let mime = if thumbnail {
        "image/png".to_string()
    } else {
        row.get::<String, _>("mime")
    };
    let length = bytes.len();
    let mut response = Body::from(bytes).into_response();
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_str(&mime).map_err(|_| ApiError::validation("格式错误"))?,
    );
    response.headers_mut().insert(
        "content-length",
        HeaderValue::from_str(&length.to_string()).unwrap(),
    );
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("private, no-store"),
    );
    if download {
        let extension = match mime.as_str() {
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => "png",
        };
        response.headers_mut().insert(
            "content-disposition",
            HeaderValue::from_str(&format!(
                "attachment; filename=\"newphoto-{id}.{extension}\""
            ))
            .unwrap(),
        );
    }
    Ok(response)
}
pub async fn preview(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(query): Query<PreviewQuery>,
) -> ApiResult<Response> {
    read(&app, auth, id, false, query.thumb.unwrap_or(false)).await
}
pub async fn download(
    State(app): State<App>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    read(&app, auth, id, true, false).await
}
