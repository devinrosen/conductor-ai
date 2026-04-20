//! Domain logic for writing attachment files to disk and augmenting agent prompts.
//!
//! The web layer owns multipart parsing and the HTTP-specific `Attachment` type
//! (which holds `bytes::Bytes`). Once bytes are available, callers convert to
//! `AttachmentFile` and delegate to this module for the filesystem operations.

use crate::error::{ConductorError, Result};

/// A resolved attachment ready for disk I/O.
pub struct AttachmentFile<'a> {
    pub filename: &'a str,
    pub mime_type: &'a str,
    pub data: &'a [u8],
}

/// Write attachment files to `{working_dir}/.conductor-attachments-{run_id}/`
/// and return the original prompt augmented with a file-path appendix.
///
/// If `attachments` is empty the original `prompt` is returned unchanged and no
/// directory is created.
pub fn write_attachments_and_augment_prompt(
    run_id: &str,
    working_dir: &str,
    prompt: &str,
    attachments: &[AttachmentFile<'_>],
) -> Result<String> {
    crate::text_util::validate_run_id(run_id)?;

    if attachments.is_empty() {
        return Ok(prompt.to_string());
    }

    let attach_dir =
        std::path::Path::new(working_dir).join(format!(".conductor-attachments-{run_id}"));
    std::fs::create_dir_all(&attach_dir).map_err(|e| {
        ConductorError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "failed to create attachment directory '{}': {e}",
                attach_dir.display()
            ),
        ))
    })?;

    let mut path_lines: Vec<String> = Vec::new();
    for att in attachments {
        // Reject filenames with path separators or traversal components.
        // This is a defence-in-depth check: the web layer sanitizes before calling
        // here, but the core function is public and must not trust its callers.
        if att.filename.contains('/')
            || att.filename.contains('\\')
            || att.filename == ".."
            || att.filename == "."
            || att.filename.is_empty()
        {
            return Err(ConductorError::InvalidInput(format!(
                "unsafe attachment filename: {:?}",
                att.filename
            )));
        }
        let file_path = attach_dir.join(att.filename);
        std::fs::write(&file_path, att.data).map_err(|e| {
            ConductorError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to write attachment '{}': {e}", file_path.display()),
            ))
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

    #[test]
    fn write_attachments_rejects_path_traversal_run_id() {
        let tmp = tempfile::tempdir().unwrap();
        let result = write_attachments_and_augment_prompt(
            "../../etc/cron.d/payload",
            tmp.path().to_str().unwrap(),
            "prompt",
            &[AttachmentFile {
                filename: "file.txt",
                mime_type: "text/plain",
                data: b"data",
            }],
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("invalid run_id"), "unexpected error: {msg}");
    }

    #[test]
    fn write_attachments_empty_returns_original_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let result = write_attachments_and_augment_prompt(
            "run-abc",
            tmp.path().to_str().unwrap(),
            "original prompt",
            &[],
        );
        assert_eq!(result.unwrap(), "original prompt");
        // No attachment directory should be created.
        assert!(!tmp.path().join(".conductor-attachments-run-abc").exists());
    }

    #[test]
    fn write_attachments_augments_prompt_with_file_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let attachments = vec![AttachmentFile {
            filename: "photo.jpg",
            mime_type: "image/jpeg",
            data: b"\xff\xd8\xff\xe0",
        }];
        let result = write_attachments_and_augment_prompt(
            "run-xyz",
            tmp.path().to_str().unwrap(),
            "describe this image",
            &attachments,
        )
        .unwrap();
        assert!(result.starts_with("describe this image\n\n---\nAttached files:\n"));
        assert!(result.contains("photo.jpg"));
        assert!(result.contains("image/jpeg"));
    }

    #[test]
    fn write_attachments_writes_file_content_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let content = b"hello world";
        let attachments = vec![AttachmentFile {
            filename: "note.txt",
            mime_type: "text/plain",
            data: content,
        }];
        write_attachments_and_augment_prompt(
            "run-123",
            tmp.path().to_str().unwrap(),
            "read this",
            &attachments,
        )
        .unwrap();
        let written = std::fs::read(
            tmp.path()
                .join(".conductor-attachments-run-123")
                .join("note.txt"),
        )
        .unwrap();
        assert_eq!(written, content);
    }

    #[test]
    fn write_attachments_rejects_path_traversal_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let result = write_attachments_and_augment_prompt(
            "run-sec",
            tmp.path().to_str().unwrap(),
            "prompt",
            &[AttachmentFile {
                filename: "../../etc/passwd",
                mime_type: "text/plain",
                data: b"data",
            }],
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("unsafe attachment filename"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn write_attachments_rejects_dotdot_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let result = write_attachments_and_augment_prompt(
            "run-sec2",
            tmp.path().to_str().unwrap(),
            "prompt",
            &[AttachmentFile {
                filename: "..",
                mime_type: "text/plain",
                data: b"data",
            }],
        );
        assert!(result.is_err());
    }

    #[test]
    fn write_attachments_rejects_dot_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let result = write_attachments_and_augment_prompt(
            "run-sec3",
            tmp.path().to_str().unwrap(),
            "prompt",
            &[AttachmentFile {
                filename: ".",
                mime_type: "text/plain",
                data: b"data",
            }],
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("unsafe attachment filename"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn write_attachments_rejects_empty_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let result = write_attachments_and_augment_prompt(
            "run-sec4",
            tmp.path().to_str().unwrap(),
            "prompt",
            &[AttachmentFile {
                filename: "",
                mime_type: "text/plain",
                data: b"data",
            }],
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("unsafe attachment filename"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn write_attachments_rejects_backslash_in_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let result = write_attachments_and_augment_prompt(
            "run-sec5",
            tmp.path().to_str().unwrap(),
            "prompt",
            &[AttachmentFile {
                filename: "evil\\file.txt",
                mime_type: "text/plain",
                data: b"data",
            }],
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("unsafe attachment filename"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn write_attachments_multiple_files_all_written() {
        let tmp = tempfile::tempdir().unwrap();
        let attachments = vec![
            AttachmentFile {
                filename: "a.txt",
                mime_type: "text/plain",
                data: b"file a",
            },
            AttachmentFile {
                filename: "b.txt",
                mime_type: "text/plain",
                data: b"file b",
            },
        ];
        let result = write_attachments_and_augment_prompt(
            "run-multi",
            tmp.path().to_str().unwrap(),
            "two files",
            &attachments,
        )
        .unwrap();
        let dir = tmp.path().join(".conductor-attachments-run-multi");
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"file a");
        assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), b"file b");
        assert!(result.contains("a.txt"));
        assert!(result.contains("b.txt"));
    }
}
