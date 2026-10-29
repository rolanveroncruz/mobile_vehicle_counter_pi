pub use sea_orm_migration::prelude::*;

mod m20220101_000001_create_table;
mod m20261017_064037_create_users;
mod m20261030_000001_add_content_hash;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20220101_000001_create_table::Migration),
            Box::new(m20261017_064037_create_users::Migration),
            Box::new(m20261030_000001_add_content_hash::Migration),
        ]
    }
}
