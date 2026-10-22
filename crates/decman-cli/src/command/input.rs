//! Parsing of command inputs: JSON bodies, DAR files, and query pairs.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::prelude::*;
use serde_json::{Value, json};

/// Read a JSON value given as inline JSON, `@path` for a file, or `-` for
/// stdin. Secrets can then come from a file or a pipe, never from the command
/// line where the shell history and process list would keep them.
///
/// # Errors
///
/// Returns an error if the file or stdin cannot be read, or the text is not
/// valid JSON.
pub fn read_json(arg: &str) -> Result<Value> {
    let (text, source) = if arg == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("failed to read JSON from stdin")?;
        (text, "stdin".to_owned())
    } else if let Some(path) = arg.strip_prefix('@') {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("failed to read {path}"))?;
        (text, path.to_owned())
    } else {
        (arg.to_owned(), "the argument".to_owned())
    };
    serde_json::from_str(&text).with_context(|| format!("{source} is not valid JSON"))
}

/// The `dar_files` entries for the DAR upload and distribute routes: each
/// file's name and its base64 content.
///
/// # Errors
///
/// Returns an error if a file cannot be read or has no file name.
pub fn dar_files(paths: &[impl AsRef<Path>]) -> Result<Vec<Value>> {
    paths
        .iter()
        .map(|path| {
            let path = path.as_ref();
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .with_context(|| format!("{} has no file name", path.display()))?;
            let bytes = std::fs::read(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            Ok(json!({ "filename": filename, "data": BASE64_STANDARD.encode(bytes) }))
        })
        .collect()
}

/// Split a `key=value` query argument.
///
/// # Errors
///
/// Returns an error if the argument has no `=` or an empty key.
pub fn query_pair(arg: &str) -> Result<(String, String)> {
    match arg.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
        _ => bail!("{arg:?} must look like KEY=VALUE"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_json_parses() {
        assert_eq!(read_json(r#"{"a":1}"#).unwrap(), json!({ "a": 1 }));
    }

    #[test]
    fn at_path_reads_the_file() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body.json");
        std::fs::write(&path, r#"[1, 2]"#).unwrap();

        // Act
        let value = read_json(&format!("@{}", path.display())).unwrap();

        // Assert
        assert_eq!(value, json!([1, 2]));
    }

    #[test]
    fn invalid_json_names_its_source() {
        let error = read_json("{nope").unwrap_err();
        assert!(error.to_string().contains("the argument"), "{error}");
    }

    #[test]
    fn dar_files_carry_name_and_base64_content() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg-1.0.0.dar");
        std::fs::write(&path, b"dar").unwrap();

        // Act
        let files = dar_files(&[&path]).unwrap();

        // Assert
        assert_eq!(
            files,
            vec![json!({ "filename": "pkg-1.0.0.dar", "data": "ZGFy" })]
        );
    }

    #[test]
    fn query_pair_splits_on_the_first_equals_sign() {
        assert_eq!(
            query_pair("party_id=a::b=c").unwrap(),
            ("party_id".to_owned(), "a::b=c".to_owned())
        );
        assert!(query_pair("novalue").is_err());
        assert!(query_pair("=x").is_err());
    }
}
