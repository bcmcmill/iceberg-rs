/*!
Defining the [ViewBuilder] struct for creating catalog views and starting create/replace transactions
*/

use std::sync::Arc;
use std::time::SystemTime;

use object_store::path::Path;
use object_store::ObjectStore;
use uuid::Uuid;

use crate::catalog::identifier::Identifier;
use crate::catalog::TableLike;
use crate::model::schema::Schema;
use crate::model::schema::SchemaV2;
use crate::model::view_metadata::{
    Operation, Representation, Summary, Version, VersionLogStruct, ViewMetadataV1,
};
use anyhow::{anyhow, Result};

use super::View;
use super::{Catalog, TableType};

///Builder pattern to create a view
pub struct ViewBuilder {
    table_type: TableType,
    metadata: ViewMetadataV1,
}

impl ViewBuilder {
    /// Creates a new [TableBuilder] to create a Metastore view with some default metadata entries already set.
    pub fn new_metastore_view(
        sql: &str,
        location: &str,
        schema: SchemaV2,
        identifier: Identifier,
        catalog: Arc<dyn Catalog>,
    ) -> Result<Self> {
        let summary = Summary {
            operation: Operation::Create,
            engine_version: None,
        };
        let representation = Representation::Sql {
            sql: sql.to_owned(),
            dialect: "ANSI".to_owned(),
            schema_id: None,
            default_catalog: None,
            default_namespace: None,
            field_aliases: None,
            field_docs: None,
        };
        let version = Version {
            version_id: 1,
            timestamp_ms: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|err| anyhow!(err.to_string()))?
                .as_millis() as i64,
            summary,
            representations: vec![representation],
        };
        let version_log = vec![VersionLogStruct {
            timestamp_ms: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|err| anyhow!(err.to_string()))?
                .as_millis() as i64,
            version_id: 1,
        }];
        let metadata = ViewMetadataV1 {
            location: location.to_string(),
            schemas: Some(vec![Schema::V2(schema)]),
            current_schema_id: Some(1),
            versions: vec![version],
            current_version_id: 1,
            version_log,
            properties: None,
        };
        Ok(ViewBuilder {
            metadata,
            table_type: TableType::Metastore(identifier, catalog),
        })
    }
    /// Creates a new [ViewBuilder] to create a FileSystem view with some default metadata entries already set.
    pub fn new_filesystem_view(
        sql: &str,
        location: &str,
        schema: SchemaV2,
        object_store: Arc<dyn ObjectStore>,
    ) -> Result<Self> {
        let summary = Summary {
            operation: Operation::Create,
            engine_version: None,
        };
        let representation = Representation::Sql {
            sql: sql.to_owned(),
            dialect: "ANSI".to_owned(),
            schema_id: None,
            default_catalog: None,
            default_namespace: None,
            field_aliases: None,
            field_docs: None,
        };
        let version = Version {
            version_id: 1,
            timestamp_ms: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|err| anyhow!(err.to_string()))?
                .as_millis() as i64,
            summary,
            representations: vec![representation],
        };
        let version_log = vec![VersionLogStruct {
            timestamp_ms: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|err| anyhow!(err.to_string()))?
                .as_millis() as i64,
            version_id: 1,
        }];
        let metadata = ViewMetadataV1 {
            location: location.to_string(),
            schemas: Some(vec![Schema::V2(schema)]),
            current_schema_id: Some(1),
            versions: vec![version],
            current_version_id: 1,
            version_log,
            properties: None,
        };
        Ok(ViewBuilder {
            metadata,
            table_type: TableType::FileSystem(object_store),
        })
    }
    /// Building a table writes the metadata file and commits the table to either the metastore or the filesystem
    pub async fn commit(self) -> Result<View> {
        match self.table_type {
            TableType::Metastore(identifier, catalog) => {
                let object_store = catalog.object_store();
                let location = &self.metadata.location;
                let uuid = Uuid::new_v4();
                let version = &self.metadata.current_version_id;
                let metadata_json = serde_json::to_string(&self.metadata)
                    .map_err(|err| anyhow!(err.to_string()))?;
                let path: Path = (location.to_string()
                    + "/metadata/"
                    + &version.to_string()
                    + "-"
                    + &uuid.to_string()
                    + ".metadata.json")
                    .into();
                object_store
                    .put(&path, metadata_json.into())
                    .await
                    .map_err(|err| anyhow!(err.to_string()))?;
                if let TableLike::View(view) =
                    catalog.register_table(identifier, path.as_ref()).await?
                {
                    Ok(view)
                } else {
                    Err(anyhow!("Building the table failed because registering the table in the catalog didn't return a table."))
                }
            }
            TableType::FileSystem(object_store) => {
                let location = &self.metadata.location;
                let uuid = Uuid::new_v4();
                let version = &self.metadata.current_version_id;
                let metadata_json = serde_json::to_string(&self.metadata)
                    .map_err(|err| anyhow!(err.to_string()))?;
                let temp_path: Path =
                    (location.to_string() + "/metadata/" + &uuid.to_string() + ".metadata.json")
                        .into();
                let final_path: Path = (location.to_string()
                    + "/metadata/v"
                    + &version.to_string()
                    + ".metadata.json")
                    .into();
                object_store
                    .put(&temp_path, metadata_json.into())
                    .await
                    .map_err(|err| anyhow!(err.to_string()))?;
                object_store
                    .copy_if_not_exists(&temp_path, &final_path)
                    .await
                    .map_err(|err| anyhow!(err.to_string()))?;
                object_store
                    .delete(&temp_path)
                    .await
                    .map_err(|err| anyhow!(err.to_string()))?;
                let view = View::load_file_system_view(location, &object_store).await?;
                Ok(view)
            }
        }
    }
}
