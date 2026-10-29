//! Adds `alpr_events.content_hash` for the collector's retry/duplicate protocol.
//!
//! `content_hash` is the lowercase hex SHA-256 of the raw POST body the collector
//! received. It is `UNIQUE`, and the Pi echo-checks it to confirm that the server
//! stored exactly what it sent.
//!
//! The column is **nullable on purpose**: rows that already exist (including the
//! ones copied by `migrate_data`) were not received through this protocol and have
//! no hash. SQLite allows any number of NULLs in a UNIQUE index, so they do not
//! conflict with each other.
//!
//! SQLite cannot add a UNIQUE *column* with `ALTER TABLE ... ADD COLUMN`, so the
//! column is added first and the uniqueness is enforced by a separate unique index.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const INDEX_NAME: &str = "idx_alpr_events_content_hash";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AlprEvents::Table)
                    .add_column(ColumnDef::new(AlprEvents::ContentHash).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(INDEX_NAME)
                    .table(AlprEvents::Table)
                    .col(AlprEvents::ContentHash)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(INDEX_NAME)
                    .table(AlprEvents::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(AlprEvents::Table)
                    .drop_column(AlprEvents::ContentHash)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum AlprEvents {
    Table,
    ContentHash,
}
