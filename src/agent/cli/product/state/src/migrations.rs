use sqlx::migrate::Migrator;

pub(crate) static MIGRATOR: Migrator = sqlx::migrate!("product/state/migrations");
pub(crate) static MEMORIES_MIGRATOR: Migrator = sqlx::migrate!("product/state/memory_migrations");
