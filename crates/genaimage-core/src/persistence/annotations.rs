use rusqlite::params;

use super::{AsyncRunRepository, RunRepository};
use crate::{
    assistant::{spec_review::SPEC_REVIEW_KIND, OutputAnnotation},
    HarnessError, Result,
};

pub(crate) const OUTPUT_ANNOTATION_LIMIT: usize = 100;

impl RunRepository {
    pub(crate) fn insert_output_annotation(
        &self,
        annotation: &OutputAnnotation,
    ) -> Result<OutputAnnotation> {
        self.guarded(|connection| {
            connection.execute(
                r#"
                INSERT INTO output_annotations (
                    output_id, run_id, kind, model, created_at,
                    caption, critique, tags_json, prompt, raw_response, detail_json
                ) VALUES (?1, ?2, ?10, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?11)
                "#,
                params![
                    annotation.output_id,
                    annotation.run_id,
                    annotation.model,
                    annotation.created_at.to_rfc3339(),
                    annotation.caption,
                    annotation.adherence_critique,
                    serde_json::to_string(&annotation.tags)?,
                    annotation.prompt,
                    annotation.raw_response,
                    annotation.kind,
                    annotation
                        .spec_review
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                ],
            )?;
            let id = connection.last_insert_rowid();
            Ok(OutputAnnotation {
                id,
                ..annotation.clone()
            })
        })
    }

    pub(crate) fn output_annotations(&self, output_id: &str) -> Result<Vec<OutputAnnotation>> {
        self.guarded_read(|connection| {
            let mut statement = connection.prepare(
                r#"
                SELECT id, output_id, run_id, kind, model, created_at,
                       caption, critique, tags_json, prompt, raw_response, detail_json
                FROM output_annotations
                WHERE output_id = ?1
                ORDER BY created_at DESC, id DESC
                LIMIT ?2
                "#,
            )?;
            let annotations = statement
                .query_map(params![output_id, OUTPUT_ANNOTATION_LIMIT as i64], |row| {
                    let kind: String = row.get(3)?;
                    if kind != "vision_review" && kind != SPEC_REVIEW_KIND {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    let detail: Option<String> = row.get(11)?;
                    let spec_review = detail
                        .map(|detail| serde_json::from_str(&detail))
                        .transpose()
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                11,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?;
                    let created_at: String = row.get(5)?;
                    let tags: String = row.get(8)?;
                    Ok(OutputAnnotation {
                        id: row.get(0)?,
                        output_id: row.get(1)?,
                        run_id: row.get(2)?,
                        kind,
                        model: row.get(4)?,
                        created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    5,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?
                            .with_timezone(&chrono::Utc),
                        caption: row.get(6)?,
                        adherence_critique: row.get(7)?,
                        tags: serde_json::from_str(&tags).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                8,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        prompt: row.get(9)?,
                        raw_response: row.get(10)?,
                        spec_review,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(HarnessError::from);
            annotations
        })
    }
}

impl AsyncRunRepository {
    pub(crate) async fn insert_output_annotation(
        &self,
        annotation: OutputAnnotation,
    ) -> Result<OutputAnnotation> {
        self.call(move |repository| repository.insert_output_annotation(&annotation))
            .await
    }

    pub(crate) async fn output_annotations(
        &self,
        output_id: &str,
    ) -> Result<Vec<OutputAnnotation>> {
        let output_id = output_id.to_owned();
        self.call(move |repository| repository.output_annotations(&output_id))
            .await
    }
}
