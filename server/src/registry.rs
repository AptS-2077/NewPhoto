use crate::{
    admin::ListQuery,
    auth::Auth,
    error::{ApiError, ApiResult},
    state::App,
};
use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub operation: String,
    pub definition: Value,
    pub installation_id: Option<Uuid>,
    pub package_id: Option<Uuid>,
    pub digest: String,
    pub version: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Registry {
    pub revision: i64,
    pub entries: Vec<Entry>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct FeatureExtensionDescriptor {
    pub id: String,
    pub hooks: Vec<String>,
    pub config_namespace: String,
}
pub fn feature_boundary() -> FeatureExtensionDescriptor {
    FeatureExtensionDescriptor {
        id: "reserved".into(),
        hooks: vec![
            "prompt".into(),
            "input".into(),
            "result".into(),
            "export".into(),
        ],
        config_namespace: "feature".into(),
    }
}
impl Registry {
    pub fn initial() -> Self {
        let entries=[("generate","OpenAI Image Generate","images/generations"),("edit","OpenAI Image Edit","images/edits")].into_iter().map(|(operation,name,path)|{
            let definition=json!({"definition_version":1,"execution":"sync_http","operation":operation,"input_slots":if operation=="edit"{vec!["image"]}else{vec![]},"request":{"method":"POST","path":path,"authentication":"deployment_bearer","encoding":if operation=="edit"{"multipart"}else{"json"},"fields":{"model":{"from":"/model/upstream_model"},"prompt":{"from":"/request/prompt"},"size":{"from":"/request/output/size"},"n":{"constant":1}},"files":if operation=="edit"{json!([{"field":"image","input_slot":"image"}])}else{json!([])}},"response":{"encoding":"json","success_http_statuses":[200],"error_pointer":"/error","images_pointer":"/data","max_images":1,"sources":[{"kind":"url","pointer":"/url"},{"kind":"base64","pointer":"/b64_json"}]}});
            Entry{id:format!("openai.images.{operation}"),name:name.into(),operation:operation.into(),definition,installation_id:None,package_id:None,digest:"builtin-v1".into(),version:"1".into()}
        }).collect();
        Self {
            revision: 1,
            entries,
        }
    }
}
pub async fn load(pool: &PgPool) -> anyhow::Result<Registry> {
    let initial = Registry::initial();
    sqlx::query("INSERT INTO registry_revisions(revision,snapshot,request_id,request_hash) VALUES(1,$1,'initial','initial') ON CONFLICT(revision) DO NOTHING").bind(serde_json::to_value(&initial)?).execute(pool).await?;
    let value: Value = sqlx::query_scalar(
        "SELECT snapshot FROM registry_revisions ORDER BY revision DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await?;
    Ok(serde_json::from_value(value)?)
}
pub async fn available_models(
    app: &App,
    auth: &Auth,
    format: Option<&str>,
) -> ApiResult<Vec<Value>> {
    let models=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('id',id,'display_name',display_name,'entry_format_id',entry_format_id,'capabilities',capabilities,'defaults',defaults,'revision',revision) FROM model_configs WHERE enabled AND deleted_at IS NULL AND ($1::text IS NULL OR entry_format_id=$1) ORDER BY display_name").bind(format).fetch_all(&app.pool).await?;
    let snapshot = app.registry.load_full();
    Ok(models
        .into_iter()
        .filter(|model| {
            model["id"]
                .as_str()
                .and_then(|id| id.parse().ok())
                .is_some_and(|id| auth.user.model(id))
                && snapshot
                    .entries
                    .iter()
                    .any(|entry| Some(entry.id.as_str()) == model["entry_format_id"].as_str())
        })
        .collect())
}
pub async fn formats(State(app): State<App>, auth: Auth) -> ApiResult<Json<Value>> {
    let models = available_models(&app, &auth, None).await?;
    let snapshot = app.registry.load_full();
    let entries:Vec<Value>=snapshot.entries.iter().map(|entry|{let available=models.iter().any(|model|model["entry_format_id"]==entry.id);json!({"id":entry.id,"name":entry.name,"operation":entry.operation,"available":available,"reason":if available{""}else{"暂无可用模型，请联系管理员"}})}).collect();
    Ok(Json(
        json!({"registry_revision":snapshot.revision,"items":entries}),
    ))
}
pub async fn models(
    State(app): State<App>,
    auth: Auth,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let (limit, offset) = query.paging();
    let items: Vec<Value> = available_models(&app, &auth, query.entry_format_id.as_deref())
        .await?
        .into_iter()
        .filter(|model| {
            model["display_name"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .contains(&query.search.as_deref().unwrap_or("").to_lowercase())
        })
        .collect();
    let has_more = items.len() > (offset + limit) as usize;
    let items: Vec<Value> = items
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    Ok(Json(
        json!({"items":items,"page":offset/limit+1,"per_page":limit,"has_more":has_more}),
    ))
}
pub fn validate_schema(schema: &Value, value: &Value) -> ApiResult<()> {
    fn safe(value: &Value) -> bool {
        match value {
            Value::Object(object) => !object.contains_key("$ref") && object.values().all(safe),
            Value::Array(array) => array.iter().all(safe),
            _ => true,
        }
    }
    if !safe(schema) {
        return Err(ApiError::validation("不支持远程或本地 $ref"));
    }
    let validator =
        jsonschema::validator_for(schema).map_err(|_| ApiError::validation("JSON Schema 无效"))?;
    validator
        .validate(value)
        .map_err(|_| ApiError::validation("配置不符合 JSON Schema"))
}
