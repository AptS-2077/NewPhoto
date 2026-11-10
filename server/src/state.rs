use crate::{config::Config, registry::Registry};
use arc_swap::ArcSwap;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
pub struct Inner {
    pub pool: PgPool,
    pub config: Config,
    pub registry: ArcSwap<Registry>,
    pub registry_lock: Mutex<()>,
    pub image_slots: Arc<Semaphore>,
    pub dummy_password: String,
}
pub type App = Arc<Inner>;
