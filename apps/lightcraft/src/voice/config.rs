//! Read the local key without modifying the process environment or logging credentials.
use std::io::Read;
use std::path::{Path, PathBuf};

const KEYS: [&str; 2] = ["OPENAI_API_KEY_ETHAN", "OPENAI_API_KEY"];
const MAX_ENV_BYTES: u64 = 64 * 1024;

pub(super) fn api_key() -> Result<String, String> {
    for name in KEYS {
        if let Ok(value) = std::env::var(name)
            && !value.trim().is_empty()
        {
            return Ok(value.trim().into());
        }
    }
    for path in candidates() {
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Could not read .env. Check its file permissions.".into()),
        };
        let mut text = String::new();
        file.take(MAX_ENV_BYTES + 1).read_to_string(&mut text).map_err(|_| ".env must be a UTF-8 text file.")?;
        if text.len() as u64 > MAX_ENV_BYTES {
            return Err("The .env file exceeds 64 KiB.".into());
        }
        if let Some(key) = parse_key(&text)? {
            return Ok(key);
        }
    }
    Err("Add OPENAI_API_KEY_ETHAN or OPENAI_API_KEY to .env in the project folder, then click Reconnect.".into())
}

fn candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(directory) = std::env::current_dir() {
        paths.push(directory.join(".env"));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        paths.push(directory.join(".env"));
        // Development builds live in <project>/target/{debug,release}. This also works
        // when the built .exe is opened from Explorer with a different working directory.
        if matches!(directory.file_name().and_then(|name| name.to_str()), Some("debug" | "release"))
            && let Some(target) = directory.parent().filter(|path| path.file_name() == Some(std::ffi::OsStr::new("target")))
            && let Some(project) = target.parent()
        {
            paths.push(project.join(Path::new(".env")));
        }
    }
    paths
}

fn parse_key(text: &str) -> Result<Option<String>, String> {
    for name in KEYS {
        for line in text.trim_start_matches('\u{feff}').lines() {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let Some((key, raw)) = line.split_once('=') else { continue };
            if key.trim() != name {
                continue;
            }
            let raw = raw.trim();
            let value = if let Some(quote @ ('\'' | '"')) = raw.chars().next() {
                let rest = raw.get(1..).ok_or("Invalid quoted API key in .env.")?;
                let (value, tail) = rest.split_once(quote).ok_or("Close the API key's quote in .env.")?;
                if !tail.trim().is_empty() && !tail.trim().starts_with('#') {
                    return Err("Unexpected text after the quoted API key in .env.".into());
                }
                value.trim()
            } else {
                raw.split_once(" #").map_or(raw, |(value, _)| value).trim()
            };
            if !value.is_empty() {
                return Ok(Some(value.into()));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::parse_key;

    #[test]
    fn local_env_accepts_bom_quotes_comments_and_legacy_key_priority() {
        let text = "\u{feff}# local settings\nOPENAI_API_KEY=fallback\nexport OPENAI_API_KEY_ETHAN = \"test-key\" # personal key\n";
        assert_eq!(parse_key(text).unwrap().as_deref(), Some("test-key"));
        assert_eq!(parse_key("OPENAI_API_KEY_ETHAN=\nOPENAI_API_KEY='fallback'").unwrap().as_deref(), Some("fallback"));
        assert_eq!(parse_key("OPENAI_API_KEY=test-key # comment").unwrap().as_deref(), Some("test-key"));
    }

    #[test]
    fn malformed_key_reports_no_secret_value() {
        let error = parse_key("OPENAI_API_KEY=\"private-key").unwrap_err();
        assert!(!error.contains("private-key"));
        assert!(parse_key("OPENAI_API_KEY='private-key' trailing").is_err());
        assert_eq!(parse_key("UNRELATED=keep-this-local").unwrap(), None);
    }
}
