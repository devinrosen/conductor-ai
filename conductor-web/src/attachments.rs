//! Multipart attachment parsing, validation, and disk I/O.
//!
//! Extracted from the `send_message` route handler so that the handler stays
//! thin (input/output only) while business logic (filename sanitisation, MIME
//! validation, file writing, prompt augmentation) lives here.

use axum::extract::{FromRequest, Multipart};

use crate::error::ApiError;
use crate::state::AppState;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const ALLOWED_MIME_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/jpg",
    "image/heic",
    "application/pdf",
    "text/plain",
];

// ── Types ─────────────────────────────────────────────────────────────────────

/// Raw attachment data parsed from a multipart field.
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    /// Stored as `bytes::Bytes` to avoid a redundant allocation when the
    /// underlying buffer is already owned by the multipart extractor.
    pub data: bytes::Bytes,
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Convert any displayable error into a 422 `ApiError`.
fn unprocessable<E: std::fmt::Display>(e: E) -> ApiError {
    ApiError::UnprocessableEntity(e.to_string())
}

/// Strip directory components and reject unsafe filenames.
///
/// Returns `None` when the input is empty, consists solely of `.` / `..`, or
/// somehow still contains a path separator after `file_name()` extraction.
/// Callers must not use the raw client-supplied value directly as a filesystem
/// path — Rust's `Path::join` does **not** block `..` or absolute paths.
pub fn sanitize_filename(raw: &str) -> Option<String> {
    let path = std::path::Path::new(raw);
    let name = path.file_name()?.to_str()?;
    // Reject anything that is empty or still contains a separator.
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return None;
    }
    Some(name.to_string())
}

/// Return `true` when `data` matches the magic bytes expected for `mime_type`.
///
/// For MIME types without a distinctive magic signature (HEIC uses a complex
/// container format; `text/plain` has none), the function falls back to a
/// secondary heuristic — validating UTF-8 for text, and passing HEIC through —
/// rather than accepting arbitrary bytes as those types.
///
/// This is a defence-in-depth check on top of the allow-list: a client that
/// claims `image/png` but sends a shell script will be rejected here even
/// though `image/png` is in `ALLOWED_MIME_TYPES`.
pub fn validate_magic_bytes(mime: &str, data: &[u8]) -> bool {
    match mime {
        "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" | "image/jpg" => data.starts_with(b"\xff\xd8"),
        "application/pdf" => data.starts_with(b"%PDF"),
        // HEIC is an ISOBMFF container — the brand code appears at bytes 4-11
        // and has many valid variants (heic, heif, mif1, msf1 …). Skip deep
        // inspection and trust the MIME allow-list instead.
        "image/heic" => true,
        // text/plain has no magic — validate UTF-8 as a proxy.
        "text/plain" => std::str::from_utf8(data).is_ok(),
        _ => false,
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Parse a `multipart/form-data` request body into a prompt string and optional
/// attachments.
///
/// * Expects a `prompt` text field (required).
/// * Zero or more fields whose names begin with `attachment` are collected as
///   file attachments. Each attachment must carry a MIME type from
///   `ALLOWED_MIME_TYPES` **and** pass a magic-byte content check.
/// * Unknown fields are silently ignored.
///
/// Returns 422 when:
/// * `prompt` is absent
/// * an attachment carries an unsupported or mismatched MIME type
pub async fn parse_multipart_body(
    request: axum::extract::Request,
    state: &AppState,
) -> Result<(String, Vec<Attachment>), ApiError> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(unprocessable)?;

    let mut prompt: Option<String> = None;
    let mut attachments: Vec<Attachment> = Vec::new();

    loop {
        let field = multipart.next_field().await.map_err(unprocessable)?;
        let Some(field) = field else { break };

        let name = field.name().unwrap_or("").to_string();

        if name == "prompt" {
            let text = field.text().await.map_err(unprocessable)?;
            prompt = Some(text);
        } else if name.starts_with("attachment") {
            // Sanitize the filename to prevent path traversal.
            // Fall back to a unique index-based name when the field carries no
            // Content-Disposition filename, ensuring multiple unnamed fields
            // don't silently collide on disk.
            let filename = match field.file_name() {
                Some(raw) => sanitize_filename(raw)
                    .ok_or_else(|| unprocessable(format!("unsafe filename: {raw}")))?,
                None => format!("attachment-{}", attachments.len()),
            };

            let mime_type = field
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string();

            if !ALLOWED_MIME_TYPES.contains(&mime_type.as_str()) {
                return Err(ApiError::UnprocessableEntity(format!(
                    "unsupported MIME type: {mime_type}"
                )));
            }

            let data = field.bytes().await.map_err(unprocessable)?;

            // Validate actual content against the claimed MIME type.
            if !validate_magic_bytes(&mime_type, &data) {
                return Err(ApiError::UnprocessableEntity(format!(
                    "attachment content does not match declared MIME type: {mime_type}"
                )));
            }

            attachments.push(Attachment {
                filename,
                mime_type,
                data,
            });
        }
        // Unknown fields are silently ignored.
    }

    let prompt = prompt
        .ok_or_else(|| ApiError::UnprocessableEntity("missing required field: prompt".into()))?;

    Ok((prompt, attachments))
}

/// Write attachment files to `{working_dir}/.conductor-attachments-{run_id}/`
/// and return the original prompt augmented with a file-path appendix.
///
/// If `attachments` is empty the original `prompt` is returned unchanged.
pub fn write_attachments_and_augment_prompt(
    run_id: &str,
    working_dir: &str,
    prompt: &str,
    attachments: &[Attachment],
) -> Result<String, ApiError> {
    if attachments.is_empty() {
        return Ok(prompt.to_string());
    }

    let attach_dir =
        std::path::Path::new(working_dir).join(format!(".conductor-attachments-{run_id}"));
    std::fs::create_dir_all(&attach_dir)
        .map_err(|e| ApiError::Internal(format!("failed to create attachment dir: {e}")))?;

    let mut path_lines: Vec<String> = Vec::new();
    for att in attachments {
        let file_path = attach_dir.join(&att.filename);
        std::fs::write(&file_path, &att.data).map_err(|e| {
            ApiError::Internal(format!("failed to write attachment {}: {e}", att.filename))
        })?;
        path_lines.push(format!("- {} ({})", file_path.display(), att.mime_type));
    }

    Ok(format!(
        "{prompt}\n\n---\nAttached files:\n{}",
        path_lines.join("\n")
    ))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── sanitize_filename ─────────────────────────────────────────────────────

    #[test]
    fn sanitize_filename_strips_directory_traversal() {
        assert_eq!(
            sanitize_filename("../../etc/cron.d/evil"),
            Some("evil".to_string())
        );
    }

    #[test]
    fn sanitize_filename_strips_absolute_path() {
        // Path::file_name() on "/etc/passwd" returns "passwd".
        assert_eq!(sanitize_filename("/etc/passwd"), Some("passwd".to_string()));
    }

    #[test]
    fn sanitize_filename_accepts_plain_name() {
        assert_eq!(
            sanitize_filename("photo.jpg"),
            Some("photo.jpg".to_string())
        );
    }

    #[test]
    fn sanitize_filename_rejects_dotdot_alone() {
        // ".." has no file_name() component.
        assert_eq!(sanitize_filename(".."), None);
    }

    #[test]
    fn sanitize_filename_rejects_empty() {
        assert_eq!(sanitize_filename(""), None);
    }

    // ── validate_magic_bytes ──────────────────────────────────────────────────

    #[test]
    fn validate_magic_bytes_accepts_valid_png() {
        let png_magic = b"\x89PNG\r\n\x1a\n followed by anything";
        assert!(validate_magic_bytes("image/png", png_magic));
    }

    #[test]
    fn validate_magic_bytes_rejects_non_png_claiming_png() {
        assert!(!validate_magic_bytes("image/png", b"GIF89a..."));
    }

    #[test]
    fn validate_magic_bytes_accepts_valid_jpeg() {
        let jpg_magic = b"\xff\xd8\xff\xe0";
        assert!(validate_magic_bytes("image/jpeg", jpg_magic));
    }

    #[test]
    fn validate_magic_bytes_accepts_valid_pdf() {
        assert!(validate_magic_bytes("application/pdf", b"%PDF-1.4 body"));
    }

    #[test]
    fn validate_magic_bytes_rejects_non_pdf_claiming_pdf() {
        assert!(!validate_magic_bytes("application/pdf", b"not a pdf"));
    }

    #[test]
    fn validate_magic_bytes_accepts_valid_utf8_text() {
        assert!(validate_magic_bytes("text/plain", b"Hello, world!"));
    }

    #[test]
    fn validate_magic_bytes_rejects_invalid_utf8_as_text() {
        assert!(!validate_magic_bytes(
            "text/plain",
            b"\xff\xfe binary garbage"
        ));
    }

    #[test]
    fn validate_magic_bytes_heic_passes_through() {
        assert!(validate_magic_bytes("image/heic", b"anything"));
    }
}
