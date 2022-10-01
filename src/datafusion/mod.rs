/*!
 * Tableprovider to use iceberg table with datafusion.
*/

use anyhow::Result;
use chrono::{naive::NaiveDateTime, DateTime, Utc};
use object_store::ObjectMeta;
use std::{any::Any, collections::HashMap, ops::DerefMut, sync::Arc};

use datafusion::{
    arrow::datatypes::SchemaRef,
    common::DataFusionError,
    datasource::{
        file_format::{parquet::ParquetFormat, FileFormat},
        listing::PartitionedFile,
        object_store::ObjectStoreUrl,
        TableProvider,
    },
    execution::context::SessionState,
    logical_expr::TableType,
    logical_plan::{combine_filters, Expr},
    physical_optimizer::pruning::PruningPredicate,
    physical_plan::{file_format::FileScanConfig, ExecutionPlan},
    scalar::ScalarValue,
};
use url::Url;

use crate::{
    datafusion::pruning_statistics::{PruneDataFiles, PruneManifests},
    table::Table,
};

use self::schema::iceberg_to_arrow_schema;

mod pruning_statistics;
mod schema;
mod statistics;
mod value;

/// Iceberg table for datafusion
pub struct DataFusionTable(Table);

impl core::ops::Deref for DataFusionTable {
    type Target = Table;

    fn deref(self: &'_ DataFusionTable) -> &'_ Self::Target {
        &self.0
    }
}

impl DerefMut for DataFusionTable {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<Table> for DataFusionTable {
    fn from(value: Table) -> Self {
        DataFusionTable(value)
    }
}

#[async_trait::async_trait]
impl TableProvider for DataFusionTable {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> SchemaRef {
        Arc::new(iceberg_to_arrow_schema(self.0.schema()).unwrap())
    }
    fn table_type(&self) -> TableType {
        TableType::Base
    }
    async fn scan(
        &self,
        session: &SessionState,
        projection: &Option<Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>, DataFusionError> {
        let schema = self.schema();

        let object_store_url = ObjectStoreUrl::parse(
            "iceberg://".to_owned() + &self.metadata().location().replace('/', "-"),
        )?;
        let url: &Url = object_store_url.as_ref();
        session.runtime_env.register_object_store(
            url.scheme(),
            url.host_str().unwrap_or_default(),
            self.0.object_store(),
        );

        let mut file_groups: HashMap<Vec<ScalarValue>, Vec<PartitionedFile>> = HashMap::new();
        if let Some(Some(predicate)) = (!filters.is_empty()).then_some(combine_filters(filters)) {
            let pruning_predicate = PruningPredicate::try_new(predicate, schema.clone())?;
            let manifests_to_prune = pruning_predicate.prune(&PruneManifests::from(self))?;
            let files = self
                .files(Some(manifests_to_prune))
                .await
                .map_err(|err| DataFusionError::Internal(format!("{}", err)))?;
            let files_to_prune = pruning_predicate.prune(&PruneDataFiles::new(self, &files))?;
            files
                .into_iter()
                .zip(files_to_prune.into_iter())
                .for_each(|(manifest, prune_file)| {
                    if !prune_file {
                        let partition_values = manifest
                            .partition_values()
                            .iter()
                            .map(|value| match value {
                                Some(v) => v.into(),
                                None => ScalarValue::Null,
                            })
                            .collect::<Vec<ScalarValue>>();
                        let object_meta = ObjectMeta {
                            location: manifest.file_path().into(),
                            size: manifest.file_size_in_bytes() as usize,
                            last_modified: {
                                let last_updated_ms = self.metadata().last_updated_ms();
                                let secs = last_updated_ms / 1000;
                                let nsecs = (last_updated_ms % 1000) as u32 * 1000000;
                                DateTime::from_utc(NaiveDateTime::from_timestamp(secs, nsecs), Utc)
                            },
                        };
                        let file = PartitionedFile {
                            object_meta,
                            partition_values,
                            range: None,
                            extensions: None,
                        };
                        file_groups
                            .entry(file.partition_values.clone())
                            .or_default()
                            .push(file);
                    };
                });
        } else {
            let files = self
                .files(None)
                .await
                .map_err(|err| DataFusionError::Internal(format!("{}", err)))?;
            files.into_iter().for_each(|manifest| {
                let partition_values = manifest
                    .partition_values()
                    .iter()
                    .map(|value| match value {
                        Some(v) => v.into(),
                        None => ScalarValue::Null,
                    })
                    .collect::<Vec<ScalarValue>>();
                let object_meta = ObjectMeta {
                    location: manifest.file_path().into(),
                    size: manifest.file_size_in_bytes() as usize,
                    last_modified: {
                        let last_updated_ms = self.metadata().last_updated_ms();
                        let secs = last_updated_ms / 1000;
                        let nsecs = (last_updated_ms % 1000) as u32 * 1000000;
                        DateTime::from_utc(NaiveDateTime::from_timestamp(secs, nsecs), Utc)
                    },
                };
                let file = PartitionedFile {
                    object_meta,
                    partition_values,
                    range: None,
                    extensions: None,
                };
                file_groups
                    .entry(file.partition_values.clone())
                    .or_default()
                    .push(file);
            });
        };

        let statistics = self
            .statistics()
            .await
            .map_err(|err| DataFusionError::Internal(format!("{}", err)))?;

        let table_partition_cols = self
            .metadata()
            .default_spec()
            .iter()
            .map(|field| field.name.clone())
            .collect();

        let file_scan_config = FileScanConfig {
            object_store_url,
            file_schema: schema,
            file_groups: file_groups.into_values().collect(),
            statistics,
            projection: projection.clone(),
            limit: limit.clone(),
            table_partition_cols,
        };
        ParquetFormat::default()
            .create_physical_plan(file_scan_config, filters)
            .await
    }
}

#[cfg(test)]
mod tests {
    use datafusion::{
        arrow::{self, record_batch::RecordBatch},
        prelude::SessionContext,
    };
    use object_store::{local::LocalFileSystem, ObjectStore};

    use super::*;

    #[tokio::test]
    pub async fn test_datafusion_scan() {
        let object_store: Arc<dyn ObjectStore> = Arc::new(
            LocalFileSystem::new_with_prefix("/home/jan/workspace/rust/iceberg-rs").unwrap(),
        );

        let table = Arc::new(DataFusionTable::from(
            Table::load_file_system_table("tests/data/nyc/taxis", &object_store)
                .await
                .unwrap(),
        ));

        let ctx = SessionContext::new();

        ctx.register_table("nyc_taxis", table).unwrap();

        let df = ctx
            .sql("SELECT vendor_id, MIN(trip_distance) FROM nyc_taxis GROUP BY vendor_id LIMIT 100")
            .await
            .unwrap();

        // execute the plan
        let results: Vec<RecordBatch> = df.collect().await.expect("Failed to execute query plan.");

        // format the results
        let pretty_results = arrow::util::pretty::pretty_format_batches(&results)
            .expect("Failed to print result")
            .to_string();
        dbg!(&pretty_results);
        panic!()
    }
}
