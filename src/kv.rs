//! Small (id, payload) LanceDB tables for entities and attachments. Business data, backed up like memories.
use anyhow::Result;
use arrow_array::{Array, RecordBatch, RecordBatchIterator, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::{
    Connection, Table,
    query::{ExecutableQuery, QueryBase, Select},
    table::AddDataMode,
};
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("payload", DataType::Utf8, false),
    ]))
}

fn batch(rows: &[(String, String)]) -> Result<RecordBatch> {
    Ok(RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.0.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.1.as_str()),
            )),
        ],
    )?)
}

fn reader(
    rows: &[(String, String)],
) -> Result<impl arrow_array::RecordBatchReader + Send + 'static + use<>> {
    Ok(RecordBatchIterator::new(vec![Ok(batch(rows)?)], schema()))
}

pub async fn open(db: &Connection, name: &str) -> Result<Table> {
    if db.table_names().execute().await?.iter().any(|n| n == name) {
        Ok(db.open_table(name).execute().await?)
    } else {
        Ok(db.create_empty_table(name, schema()).execute().await?)
    }
}

pub async fn payloads(table: &Table) -> Result<Vec<String>> {
    let batches: Vec<RecordBatch> = table
        .query()
        .select(Select::columns(&["payload"]))
        .execute()
        .await?
        .try_collect()
        .await?;
    let mut out = Vec::new();
    for batch in batches {
        if let Some(col) = batch
            .column_by_name("payload")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        {
            out.extend(col.iter().flatten().map(str::to_owned));
        }
    }
    Ok(out)
}

/// Insert or replace rows by id.
pub async fn put(table: &Table, rows: &[(String, String)]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut op = table.merge_insert(&["id"]);
    op.when_matched_update_all(None)
        .when_not_matched_insert_all();
    op.execute(Box::new(reader(rows)?)).await?;
    Ok(())
}

pub async fn delete(table: &Table, id: &str) -> Result<()> {
    table
        .delete(&format!("id = '{}'", id.replace('\'', "''")))
        .await?;
    Ok(())
}

/// Replace the whole table content.
pub async fn replace(table: &Table, rows: &[(String, String)]) -> Result<()> {
    if rows.is_empty() {
        table.delete("true").await?;
        return Ok(());
    }
    table
        .add(batch(rows)?)
        .mode(AddDataMode::Overwrite)
        .execute()
        .await?;
    Ok(())
}
