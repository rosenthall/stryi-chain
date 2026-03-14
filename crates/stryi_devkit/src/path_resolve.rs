use std::path::{Path, PathBuf};
use std::{env, fs};

/// Directory that contains the given file path. Falls back to "." if no parent.
pub fn config_base_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Expand leading "~" to $HOME. Useful in configs (shells don't expand inside TOML).
pub fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if (s == "~" || s.starts_with("~/"))
        && let Ok(home) = env::var("HOME")
    {
        let mut p = PathBuf::from(home);
        if s.len() > 1 {
            // skip "~/"
            p.push(&s[2..]);
        }
        return p;
    }
    path.to_path_buf()
}

/// Resolve `candidate` against `base_dir` (if relative), expand "~", and canonicalize if it exists.
/// If it doesn't exist yet, return the joined path without failing.
pub fn resolve_relative(base_dir: &Path, candidate: &Path) -> PathBuf {
    let candidate = expand_tilde(candidate);
    let joined = if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    };

    match fs::metadata(&joined) {
        Ok(_) => fs::canonicalize(&joined).unwrap_or(joined),
        Err(_) => joined,
    }
}
