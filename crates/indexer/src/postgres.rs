//! Authoritative-source adapter that reads artifact versions from PostgreSQL.
//!
//! This is a **read-only** projection: it selects artifact metadata and immutable
//! version rows that CORE-007 owns and pairs them with text read through an
//! [`ObjectTextProvider`]. It never writes artifact state, so the index cannot
//! become a second database of record.

use async_trait::async_trait;
use sqlx::{PgPool, Row};

use crate::document::SourceDocument;
use crate::error::{IndexError, IndexResult};
use crate::source::{AuthoritativeSource, ObjectTextProvider, SourceScope};

/// SQL selecting the authoritative artifact-version rows visible to one tenant.
const LOAD_SQL: &str = "\
SELECT a.id            AS artifact_id, \
       a.workspace_id  AS workspace_id, \
       a.kind          AS artifact_kind, \
       a.title         AS title, \
       v.id            AS version_id, \
       v.content_digest AS content_digest, \
       v.media_type    AS media_type, \
       v.object_key    AS object_key \
  FROM artifacts a \
  JOIN artifact_versions v ON v.artifact_id = a.id \
 WHERE a.tenant_id = $1 \
   AND v.tenant_id = $1 \
   AND a.deleted_at IS NULL \
   AND ($2::text IS NULL OR a.workspace_id = $2::text) \
 ORDER BY a.id, v.seq";

/// Reads artifact-version projections for a tenant, with text from stored bytes.
pub struct PostgresArtifactSource<P> {
    pool: PgPool,
    text: P,
}

impl<P> PostgresArtifactSource<P> {
    /// Build the source over a pool and an object-text provider.
    #[must_use]
    pub fn new(pool: PgPool, text: P) -> Self {
        Self { pool, text }
    }
}

#[async_trait]
impl<P: ObjectTextProvider> AuthoritativeSource for PostgresArtifactSource<P> {
    async fn load(&self, scope: &SourceScope) -> IndexResult<Vec<SourceDocument>> {
        let mut transaction = self.pool.begin().await.map_err(source_error)?;
        // Tenant context is local to this read transaction, so it can never leak to
        // another tenant reusing the pooled connection.
        sqlx::query("SELECT set_config('quansio.tenant_id', $1, true)")
            .bind(&scope.tenant_id)
            .execute(&mut *transaction)
            .await
            .map_err(source_error)?;
        let rows = sqlx::query(LOAD_SQL)
            .bind(&scope.tenant_id)
            .bind(scope.workspace_id.as_deref())
            .fetch_all(&mut *transaction)
            .await
            .map_err(source_error)?;
        transaction.commit().await.map_err(source_error)?;

        let mut documents = Vec::with_capacity(rows.len());
        for row in rows {
            let object_key: String = text_column(&row, "object_key")?;
            let media_type: String = text_column(&row, "media_type")?;
            let title: String = text_column(&row, "title")?;
            let body = self
                .text
                .text_for(&object_key, &media_type)
                .await?
                .unwrap_or_default();
            let mut document = SourceDocument::artifact(
                scope.tenant_id.clone(),
                text_column::<String>(&row, "workspace_id")?,
                text_column::<String>(&row, "artifact_id")?,
                text_column::<String>(&row, "version_id")?,
                title,
                body,
                media_type,
                text_column::<String>(&row, "content_digest")?,
            );
            document.artifact_kind = Some(text_column(&row, "artifact_kind")?);
            documents.push(document);
        }
        Ok(documents)
    }
}

fn text_column<T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>>(
    row: &sqlx::postgres::PgRow,
    column: &str,
) -> IndexResult<T> {
    row.try_get(column)
        .map_err(|error| IndexError::Source(error.to_string()))
}

fn source_error(error: sqlx::Error) -> IndexError {
    IndexError::Source(error.to_string())
}
