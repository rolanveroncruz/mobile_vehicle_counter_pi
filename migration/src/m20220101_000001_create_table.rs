use sea_orm_migration::{prelude::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        Self::create_location_table(manager).await?;
        Self::create_sublocation_table(manager).await?;
        Self::create_camera_table(manager).await?;
        Self::create_alpr_events_table(manager).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        Self::drop_alpr_events_table(manager).await?;
        Self::drop_camera_table(manager).await?;
        Self::drop_sublocation_table(manager).await?;
        Self::drop_location_table(manager).await
    }
}

impl Migration {
    pub async fn create_location_table(manager: &SchemaManager<'_>)->Result<(), DbErr>{
        manager
            .create_table(
                Table::create()
                    .table(Location::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Location::Id)
                        .string()
                        .not_null()
                        .primary_key()
                    )
                    .col(ColumnDef::new(Location::Name)
                        .string()
                        .not_null()
                    )
                    .col(ColumnDef::new(Location::Description)
                        .string()
                        .not_null()
                    )
                    .to_owned()
            )
            .await
    }
    pub async fn drop_location_table(manager: &SchemaManager<'_>)->Result<(), DbErr>{
        manager
            .drop_table(
                Table::drop()
                    .table(Location::Table).to_owned()
            ).await
    }
    pub async fn create_sublocation_table(manager: &SchemaManager<'_>)->Result<(), DbErr>{
        manager
            .create_table(
                Table::create()
                    .table(Sublocation::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Sublocation::Id)
                        .string()
                        .not_null()
                        .primary_key()
                    )
                    .col(ColumnDef::new(Sublocation::Name)
                        .string()
                        .not_null()
                    )
                    .col(ColumnDef::new(Sublocation::Description)
                        .string()
                    )
                    .col(ColumnDef::new(Sublocation::LocationId)
                        .string()
                        .not_null()
                    )
                    .foreign_key(ForeignKey::create()
                        .name("fk_sublocation_location_id")
                        .from(Sublocation::Table, Sublocation::LocationId)
                        .to(Location::Table, Location::Id)
                        .on_delete(ForeignKeyAction::Cascade)
                        .on_update(ForeignKeyAction::Cascade)
                    )
                    .to_owned()
            ).await
    }
    pub async fn drop_sublocation_table(manager: &SchemaManager<'_>)->Result<(), DbErr>{
        manager
            .drop_table(
                Table::drop()
                    .table(Sublocation::Table).to_owned()
            ).await
    }
    async fn create_camera_table(manager: &SchemaManager<'_>) ->Result<(), DbErr> {
        manager.create_table(
            Table::create()
                .table(Camera::Table)
                .if_not_exists()
                .col(ColumnDef::new(Camera::Id)
                    .string()
                    .not_null()
                    .primary_key()
                )
                .col(ColumnDef::new(Camera::Name)
                    .string()
                    .not_null()
                )
                .col(ColumnDef::new(Camera::Description)
                    .string()
                    .not_null()
                )
                .col(ColumnDef::new(Camera::SublocationId)
                    .string()
                    .not_null()
                )
                .foreign_key(ForeignKey::create()
                    .name("fk_camera_sublocation_id")
                    .from(Camera::Table, Camera::SublocationId)
                    .to(Sublocation::Table, Sublocation::Id)
                    .on_delete(ForeignKeyAction::Restrict)
                    .on_update(ForeignKeyAction::Cascade)
                )
                .col(ColumnDef::new(Camera::Orientation)
                    .string()
                    .not_null()
                )
                .check(
                    Expr::col(Camera::Orientation).is_in(["Inbound", "Outbound"])
                )
                .to_owned(),
        ).await
    }
    async fn drop_camera_table(manager: &SchemaManager<'_>) ->Result<(), DbErr>{
    manager
        .drop_table(Table::drop().table(Camera::Table).to_owned())
        .await

}

    async fn create_alpr_events_table(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AlprEvents::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(AlprEvents::Id)
                        .string()
                        .not_null()
                        .primary_key()
                    )
                    .col(ColumnDef::new(AlprEvents::CameraId)
                        .string()
                        .not_null()
                    )
                    .foreign_key(ForeignKey::create()
                        .name("fk-alpr_event-camera_id")
                        .from(AlprEvents::Table, AlprEvents::CameraId)
                        .to(Camera::Table, Camera::Id)
                        .on_delete(ForeignKeyAction::Restrict)
                        .on_update(ForeignKeyAction::Restrict)
                    )
                    .col(ColumnDef::new(AlprEvents::Date)
                        .timestamp_with_time_zone()
                        .not_null()
                    )
                    .col(ColumnDef::new(AlprEvents::Plate)
                        .string()
                        .not_null()
                    )
                    .col(ColumnDef::new(AlprEvents::Image)
                        .string()
                        .not_null()
                    )
                    .col(ColumnDef::new(AlprEvents::PlateImage)
                        .string()
                        .not_null()
                    )
                    .to_owned(),
            )
            .await
    }
    async fn drop_alpr_events_table(manager: &SchemaManager<'_>) ->Result<(), DbErr>{
        manager
            .drop_table(Table::drop().table(AlprEvents::Table).to_owned())
            .await
    }



}

#[derive(DeriveIden)]
enum Location{
    Table,
    Id,
    Name,
    Description,
}
#[derive(DeriveIden)]
enum Sublocation{
    Table,
    Id,
    Name,
    Description,
    LocationId,
}

#[derive(DeriveIden)]
enum Camera{
    Table,
    Id,
    Name,
    Description,
    SublocationId,
    Orientation,
}


#[derive(DeriveIden)]
enum AlprEvents {
    Table,
    Id,
    CameraId,
    Date,
    Plate,
    Image,
    PlateImage,

}