use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum EnvFileError {
    #[error("failed to read env file '{path}': {source}")]
    ReadError {
        path: String,
        source: std::io::Error,
    },
}

fn strip_quotes(s: &str) -> String {
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
        {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

pub fn parse_env_contents(contents: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();

    for line in contents.lines() {
        let trimmed = line.trim();

        // Skip blank lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Split on first '='
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };

        let key = key.trim().to_string();
        if key.is_empty() {
            continue;
        }

        let value = strip_quotes(value.trim());
        map.insert(key, value);
    }

    map
}

pub fn load_env_file(path: &Path) -> Result<HashMap<String, String>, EnvFileError> {
    let contents = std::fs::read_to_string(path).map_err(|e| EnvFileError::ReadError {
        path: path.display().to_string(),
        source: e,
    })?;
    Ok(parse_env_contents(&contents))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_key_value() {
        let input = "FOO=bar\nBAZ=qux";
        let map = parse_env_contents(input);
        assert_eq!(map.get("FOO").unwrap(), "bar");
        assert_eq!(map.get("BAZ").unwrap(), "qux");
    }

    #[test]
    fn test_comments_and_blank_lines() {
        let input = "# this is a comment\n\nFOO=bar\n  # another comment\n\nBAZ=qux\n";
        let map = parse_env_contents(input);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("FOO").unwrap(), "bar");
        assert_eq!(map.get("BAZ").unwrap(), "qux");
    }

    #[test]
    fn test_double_quoted_value() {
        let input = "FOO=\"hello world\"";
        let map = parse_env_contents(input);
        assert_eq!(map.get("FOO").unwrap(), "hello world");
    }

    #[test]
    fn test_single_quoted_value() {
        let input = "FOO='hello world'";
        let map = parse_env_contents(input);
        assert_eq!(map.get("FOO").unwrap(), "hello world");
    }

    #[test]
    fn test_empty_value() {
        let input = "FOO=";
        let map = parse_env_contents(input);
        assert_eq!(map.get("FOO").unwrap(), "");
    }

    #[test]
    fn test_value_with_equals() {
        let input = "DATABASE_URL=postgres://user:pass@host/db?opt=val";
        let map = parse_env_contents(input);
        assert_eq!(
            map.get("DATABASE_URL").unwrap(),
            "postgres://user:pass@host/db?opt=val"
        );
    }

    #[test]
    fn test_whitespace_trimming() {
        let input = "  FOO  =  bar  ";
        let map = parse_env_contents(input);
        assert_eq!(map.get("FOO").unwrap(), "bar");
    }

    #[test]
    fn test_missing_file_error() {
        let result = load_env_file(Path::new("/nonexistent/.env"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("/nonexistent/.env"));
    }
}
