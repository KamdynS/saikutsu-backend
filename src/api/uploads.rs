use axum::{
    extract::{Multipart, Path, State},
    Extension, Json,
};
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    api::AuthUser,
    error::{AppError, AppResult},
    models::{UploadResponse, UploadPreview, FinalizeUploadRequest},
    services::upload_service,
    AppState,
};

pub async fn create(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    mut multipart: Multipart,
) -> AppResult<Json<UploadResponse>> {
    let mut filename: Option<String> = None;
    let mut language: Option<String> = None;
    let mut pdf_bytes: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().map(|s| s.to_string());
        let file_name = field.file_name().map(|s| s.to_string());

        match field_name.as_deref() {
            Some("file") => {
                filename = file_name;
                let bytes = field.bytes().await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read file: {}", e)))?;
                pdf_bytes = Some(bytes.to_vec());
            }
            Some("language") => {
                let text = field.text().await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read language: {}", e)))?;
                language = Some(text);
            }
            _ => {}
        }
    }

    let filename = filename.ok_or_else(|| AppError::BadRequest("No file provided".to_string()))?;
    let language = language.unwrap_or_else(|| "ja".to_string());
    let pdf_bytes = pdf_bytes.ok_or_else(|| AppError::BadRequest("No file data".to_string()))?;

    // Validate file extension
    if !filename.to_lowercase().ends_with(".pdf") {
        return Err(AppError::BadRequest("Only PDF files are supported".to_string()));
    }

    // Create upload record
    let upload = upload_service::create_upload(
        &state.db,
        auth_user.user_id,
        &filename,
        &language,
    ).await?;

    let upload_id = upload.id;

    // Process PDF in background
    let pool = state.db.clone();
    let lang = language.clone();
    tokio::spawn(async move {
        if let Err(e) = upload_service::process_pdf(&pool, upload_id, &pdf_bytes, &lang).await {
            tracing::error!("PDF processing failed for {}: {}", upload_id, e);
            let _ = sqlx::query(
                "UPDATE uploads SET status = 'failed', error_message = $1 WHERE id = $2"
            )
            .bind(e.to_string())
            .bind(upload_id)
            .execute(&pool)
            .await;
        }
    });

    Ok(Json(UploadResponse::from(upload)))
}

pub async fn get_status(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<UploadResponse>> {
    let upload = upload_service::get_upload(&state.db, id, auth_user.user_id).await?;
    Ok(Json(UploadResponse::from(upload)))
}

pub async fn preview(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<UploadPreview>> {
    let preview = upload_service::get_upload_preview(&state.db, id, auth_user.user_id).await?;
    Ok(Json(preview))
}

#[derive(serde::Serialize)]
pub struct FinalizeResponse {
    pub deck_id: Uuid,
}

pub async fn finalize(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<FinalizeUploadRequest>,
) -> AppResult<Json<FinalizeResponse>> {
    let deck_id = upload_service::finalize_upload(
        &state.db,
        id,
        auth_user.user_id,
        &req.deck_name,
        &req.selected_words,
    ).await?;

    Ok(Json(FinalizeResponse { deck_id }))
}
