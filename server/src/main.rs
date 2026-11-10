mod admin;
mod auth;
mod config;
mod error;
mod net;
mod plugins;
mod registry;
mod state;
mod storage;
mod tasks;
mod worker;

use anyhow::{Context, Result, bail};
use arc_swap::ArcSwap;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::HeaderValue,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, Semaphore};
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "newphoto", version, about = "NewPhoto 图片工作台")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    Serve,
    Migrate,
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
    Recovery {
        #[arg(value_parser=["pause","resume"])]
        action: String,
    },
}
#[derive(Subcommand)]
enum KeyCommand {
    Generate {
        #[arg(long, default_value = "secrets/master.key")]
        output: PathBuf,
    },
}
#[derive(Subcommand)]
enum AdminCommand {
    Create {
        #[arg(long)]
        username: String,
        #[arg(long, default_value = "管理员")]
        display_name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "newphoto=info,tower_http=warn".into()),
        )
        .init();
    let command = Cli::parse().command.unwrap_or(Command::Serve);
    if let Command::Key {
        command: KeyCommand::Generate { output },
    } = command
    {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&output)
            .context("key file already exists or cannot be created")?;
        use std::io::Write;
        file.write_all(STANDARD.encode(rand::random::<[u8; 32]>()).as_bytes())?;
        file.sync_all()?;
        println!("已生成主密钥文件；请单独备份，不要提交到仓库");
        return Ok(());
    }
    let config = config::Config::load()?;
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&config.database_url)
        .await
        .context("connect PostgreSQL")?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    for directory in ["images", "thumbnails", "receipts", "plugins"] {
        tokio::fs::create_dir_all(config.data_dir.join(directory)).await?;
    }
    match command {
        Command::Migrate => {
            println!("数据库迁移完成");
            return Ok(());
        }
        Command::Admin {
            command:
                AdminCommand::Create {
                    username,
                    display_name,
                },
        } => {
            let username = auth::validate_username(&username)
                .map_err(|error| anyhow::anyhow!(error.message))?;
            let password = match std::env::var("NEWPHOTO_ADMIN_PASSWORD") {
                Ok(password) => password,
                Err(_) => rpassword::prompt_password("初始密码（12–128 字符）: ")?,
            };
            let password_hash = auth::password_hash(password)
                .await
                .map_err(|error| anyhow::anyhow!(error.message))?;
            let mut transaction = pool.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(734201)")
                .execute(&mut *transaction)
                .await?;
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE role='admin' AND status='enabled' AND deleted_at IS NULL)").fetch_one(&mut *transaction).await?;
            if exists {
                bail!("已有管理员；后续管理员请从管理后台创建");
            }
            let id = Uuid::new_v4();
            let identity = Uuid::new_v4();
            sqlx::query("INSERT INTO users(id,username,display_name,role,model_access) VALUES($1,$2,$3,'admin','all_enabled')").bind(id).bind(&username).bind(display_name).execute(&mut *transaction).await?;
            sqlx::query("INSERT INTO auth_identities VALUES($1,$2,'local_password',$3)")
                .bind(identity)
                .bind(id)
                .bind(username)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("INSERT INTO local_credentials VALUES($1,$2)")
                .bind(identity)
                .bind(password_hash)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            println!("首个管理员创建完成");
            return Ok(());
        }
        Command::Recovery { action } => {
            sqlx::query("UPDATE app_settings SET value=$1 WHERE key='generation_paused'")
                .bind(json!(action == "pause"))
                .execute(&pool)
                .await?;
            println!(
                "外部生成已{}",
                if action == "pause" {
                    "暂停；请核对历史任务"
                } else {
                    "恢复"
                }
            );
            return Ok(());
        }
        _ => {}
    }
    let mut singleton = pool.acquire().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(734202)")
        .fetch_one(&mut *singleton)
        .await?;
    if !locked {
        bail!("V1 只允许一个服务实例");
    }
    let registry = registry::load(&pool).await?;
    let app = Arc::new(state::Inner {
        pool,
        config: config.clone(),
        registry: ArcSwap::from_pointee(registry),
        registry_lock: Mutex::new(()),
        image_slots: Arc::new(Semaphore::new(2)),
        dummy_password: auth::password_hash(auth::random_token())
            .await
            .map_err(|error| anyhow::anyhow!(error.message))?,
    });
    worker::recover(&app).await?;
    let worker = tokio::spawn(worker::run(app.clone()));
    let maintenance_app = app.clone();
    let maintenance = tokio::spawn(async move {
        loop {
            if let Err(error) = storage::cleanup_unreferenced(&maintenance_app).await {
                tracing::warn!(code = error.code, "temporary file cleanup failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
    let api = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/me", get(auth::me))
        .route("/auth/logout", post(auth::logout))
        .route("/me", get(auth::me).patch(auth::update_me))
        .route("/entry-formats", get(registry::formats))
        .route("/models", get(registry::models))
        .route("/assets/inputs", post(storage::upload))
        .route("/assets/{id}/preview", get(storage::preview))
        .route("/assets/{id}/download", get(storage::download))
        .route("/tasks", get(tasks::list).post(tasks::create))
        .route("/tasks/{id}", get(tasks::detail))
        .route("/tasks/{id}/upstream-requests", get(tasks::upstream))
        .route("/images", get(tasks::images))
        .route("/admin/users", get(admin::users).post(admin::create_user))
        .route(
            "/admin/users/{id}",
            get(admin::user_detail)
                .patch(admin::update_user)
                .delete(admin::delete_user),
        )
        .route(
            "/admin/users/{id}/reset-password",
            post(admin::reset_password),
        )
        .route(
            "/admin/models",
            get(admin::models).post(admin::create_model),
        )
        .route(
            "/admin/models/{id}",
            get(admin::model_detail)
                .patch(admin::update_model)
                .delete(admin::delete_model),
        )
        .route("/admin/models/{id}/test", post(tasks::test_model))
        .route("/admin/plugins", get(plugins::list))
        .route("/admin/plugins/install", post(plugins::install))
        .route(
            "/admin/plugins/{id}",
            get(plugins::detail)
                .patch(plugins::update)
                .delete(plugins::remove),
        )
        .route("/admin/registry/refresh", post(plugins::refresh))
        .fallback(|| async { error::ApiError::missing() });
    let router = Router::new()
        .nest("/api/v1", api)
        .route(
            "/health/live",
            get(|| async { Json(json!({"status":"live"})) }),
        )
        .route("/health/ready", get(ready))
        .fallback_service(
            ServeDir::new(&config.web_dir)
                .not_found_service(ServeFile::new(config.web_dir.join("index.html"))),
        )
        .layer(DefaultBodyLimit::max(41 * 1024 * 1024))
        .layer(middleware::from_fn(security_headers))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(bind=%config.bind,auth_provider=%auth::provider_metadata(),feature_namespace=%registry::feature_boundary().config_namespace,"NewPhoto started");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    worker.abort();
    maintenance.abort();
    drop(singleton);
    Ok(())
}
async fn ready(State(app): State<state::App>) -> Result<Json<Value>, error::ApiError> {
    sqlx::query("SELECT 1").execute(&app.pool).await?;
    let path = app
        .config
        .data_dir
        .join(format!(".health-{}", Uuid::new_v4()));
    tokio::fs::write(&path, b"ready").await?;
    tokio::fs::remove_file(path).await?;
    Ok(Json(json!({"status":"ready"})))
}
async fn security_headers(request: Request, next: Next) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if is_api
        && response.status().is_client_error()
        && !response
            .headers()
            .get("content-type")
            .is_some_and(|value| value.as_bytes().starts_with(b"application/json"))
    {
        response = error::ApiError::new(
            response.status(),
            "VALIDATION_ERROR",
            "请求格式错误、字段类型不匹配或请求体超限",
        )
        .into_response();
    }
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("cache-control", "no-store"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' blob:; connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}
