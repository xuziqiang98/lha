use sqlx::migrate::Migrator;

pub(crate) static MIGRATOR: Migrator = sqlx::migrate!("./migrations");
pub(crate) static MEMORIES_MIGRATOR: Migrator = sqlx::migrate!("./memory_migrations");
