//! Filesystem and browser helpers.

use std::path::{Path, PathBuf};

/// Resolve `path` against `cwd` when it is relative. Absolute paths pass
/// through. Anchoring to the invocation directory (not the repository root)
/// is what a user typing a relative path expects.
pub fn resolve_relative(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// A `file://` URL for `path`, percent-encoding the characters that break
/// browsers. Spaces are the common case; `#` and `?` would truncate the URL.
pub fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// Open `url` in the user's browser, best effort. A failure is never fatal:
/// the caller has already printed the path.
pub fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    let _ = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_anchor_to_cwd() {
        let got = resolve_relative(Path::new("/work"), Path::new("out/plan.html"));
        assert_eq!(got, PathBuf::from("/work/out/plan.html"));
    }

    #[test]
    fn absolute_paths_pass_through() {
        let got = resolve_relative(Path::new("/work"), Path::new("/tmp/plan.html"));
        assert_eq!(got, PathBuf::from("/tmp/plan.html"));
    }

    #[test]
    fn file_url_escapes_spaces_and_fragments() {
        assert_eq!(file_url(Path::new("/a b/c.html")), "file:///a%20b/c.html");
        assert_eq!(file_url(Path::new("/a#b.html")), "file:///a%23b.html");
    }
}
