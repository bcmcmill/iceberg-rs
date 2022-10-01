/*!
 * Helper for iterating over files in a table.
*/
use std::{io::Cursor, iter::repeat, sync::Arc};

use anyhow::Result;
use apache_avro::types::Value as AvroValue;
use futures::{stream, StreamExt, TryFutureExt, TryStreamExt};
use object_store::path::Path;

use crate::model::{manifest::ManifestEntry, manifest_list::ManifestFile};

use super::Table;

impl Table {
    /// Get a stream of files associated to a table. The files are returned based on the list of manifest files associated to the table.
    /// The included manifest files can be filtered based on an filter vector. The filter vector has the length equal to the number of manifest files
    /// and contains a true entry everywhere the manifest file is to be included in the output.
    pub async fn files(&self, filter: Option<Vec<bool>>) -> Result<Vec<ManifestEntry>> {
        let iter = match filter {
            Some(predicate) => {
                self.manifests()
                    .iter()
                    .zip(Box::new(predicate.into_iter())
                        as Box<dyn Iterator<Item = bool> + Send + Sync>)
                    .filter_map(
                        filter_manifest as fn((&ManifestFile, bool)) -> Option<&ManifestFile>,
                    )
            }
            None => self
                .manifests()
                .iter()
                .zip(Box::new(repeat(true)) as Box<dyn Iterator<Item = bool> + Send + Sync>)
                .filter_map(filter_manifest as fn((&ManifestFile, bool)) -> Option<&ManifestFile>),
        };
        stream::iter(iter)
            .map(|file| async move {
                let object_store = Arc::clone(&self.object_store());
                let path: Path = file.manifest_path().into();
                let bytes = Cursor::new(Vec::from(
                    object_store
                        .get(&path)
                        .and_then(|file| file.bytes())
                        .await?,
                ));
                let reader = apache_avro::Reader::new(bytes)?;
                Ok(stream::iter(reader.map(
                    avro_value_to_manifest_entry
                        as fn(
                            Result<AvroValue, apache_avro::Error>,
                        ) -> Result<ManifestEntry, anyhow::Error>,
                )))
            })
            .flat_map(|reader| reader.try_flatten_stream())
            .try_collect()
            .await
    }
}

fn filter_manifest((manifest, predicate): (&ManifestFile, bool)) -> Option<&ManifestFile> {
    if predicate {
        Some(manifest)
    } else {
        None
    }
}

fn avro_value_to_manifest_entry(
    entry: Result<AvroValue, apache_avro::Error>,
) -> Result<ManifestEntry, anyhow::Error> {
    entry
        .and_then(|value| apache_avro::from_value(&value))
        .map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {

    use object_store::{memory::InMemory, ObjectStore};
    use std::sync::Arc;

    use crate::{
        model::schema::{AllType, PrimitiveType, SchemaStruct, SchemaV2, StructField},
        table::table_builder::TableBuilder,
    };

    #[tokio::test]
    async fn test_files_stream() {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let schema = SchemaV2 {
            schema_id: 1,
            identifier_field_ids: Some(vec![1, 2]),
            name_mapping: None,
            struct_fields: SchemaStruct {
                fields: vec![
                    StructField {
                        id: 1,
                        name: "one".to_string(),
                        required: false,
                        field_type: AllType::Primitive(PrimitiveType::String),
                        doc: None,
                    },
                    StructField {
                        id: 2,
                        name: "two".to_string(),
                        required: false,
                        field_type: AllType::Primitive(PrimitiveType::String),
                        doc: None,
                    },
                ],
            },
        };
        let mut table =
            TableBuilder::new_filesystem_table("test/append", schema, Arc::clone(&object_store))
                .unwrap()
                .commit()
                .await
                .unwrap();

        table
            .new_transaction()
            .fast_append(vec![
                "test/append/data/file1.parquet".to_string(),
                "test/append/data/file2.parquet".to_string(),
            ])
            .commit()
            .await
            .unwrap();
        table
            .new_transaction()
            .fast_append(vec![
                "test/append/data/file3.parquet".to_string(),
                "test/append/data/file4.parquet".to_string(),
            ])
            .commit()
            .await
            .unwrap();
        let mut files = table
            .files(None)
            .await
            .unwrap()
            .into_iter()
            .map(|manifest_entry| manifest_entry.file_path().to_string());
        assert_eq!(
            files.next().unwrap(),
            "test/append/data/file1.parquet".to_string()
        );
        assert_eq!(
            files.next().unwrap(),
            "test/append/data/file2.parquet".to_string()
        );
        assert_eq!(
            files.next().unwrap(),
            "test/append/data/file3.parquet".to_string()
        );
        assert_eq!(
            files.next().unwrap(),
            "test/append/data/file4.parquet".to_string()
        );
    }
}
