use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

pub fn resolve_command_path(command: &str) -> PathBuf {
    if command.trim().is_empty() {
        return PathBuf::new();
    }

    let command_path = Path::new(command);
    if is_explicit_path(command, command_path) {
        return command_path.to_path_buf();
    }

    let path_entries = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();

    if let Some(found) = find_in_path_entries(command, &path_entries) {
        return found;
    }

    if is_codex_command(command) {
        let home_roots = home_roots();
        if let Some(found) = find_bundled_codex_in_roots(&home_roots) {
            return found;
        }
    }

    command_path.to_path_buf()
}

fn is_explicit_path(command: &str, path: &Path) -> bool {
    path.is_absolute()
        || path.components().count() > 1
        || command.contains('\\')
        || command.as_bytes().get(1) == Some(&b':')
}

fn is_codex_command(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .and_then(OsStr::to_str)
        .map(|name| matches!(name, "codex" | "codex.exe" | "codex.cmd" | "codex.bat"))
        .unwrap_or(false)
}

fn find_in_path_entries(command: &str, path_entries: &[PathBuf]) -> Option<PathBuf> {
    let file_name = Path::new(command).file_name()?.to_os_string();
    let variants = executable_name_variants(&file_name);

    path_entries.iter().find_map(|dir| {
        variants.iter().find_map(|name| {
            let candidate = dir.join(name);
            if candidate.is_file() {
                Some(candidate)
            } else {
                None
            }
        })
    })
}

fn executable_name_variants(file_name: &OsStr) -> Vec<OsString> {
    let mut variants = vec![file_name.to_os_string()];
    if cfg!(windows) && Path::new(file_name).extension().is_none() {
        for suffix in [".exe", ".cmd", ".bat"] {
            let mut variant = file_name.to_os_string();
            variant.push(suffix);
            variants.push(variant);
        }
    }
    variants
}

fn home_roots() -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut roots = Vec::new();

    for value in [
        std::env::var_os("HOME"),
        std::env::var_os("USERPROFILE"),
        windows_home_from_username(),
    ]
    .into_iter()
    .flatten()
    {
        let path = PathBuf::from(value);
        if seen.insert(path.clone()) {
            roots.push(path);
        }
    }

    roots
}

fn windows_home_from_username() -> Option<OsString> {
    if !cfg!(target_os = "linux") {
        return None;
    }

    let username = std::env::var_os("USERNAME")?;
    let mut path = PathBuf::from("/mnt/c/Users");
    path.push(username);
    Some(path.into_os_string())
}

fn find_bundled_codex_in_roots(roots: &[PathBuf]) -> Option<PathBuf> {
    let mut extensions = Vec::new();

    for root in roots {
        for base in [
            root.join(".vscode/extensions"),
            root.join(".vscode-server/extensions"),
        ] {
            let entries = match fs::read_dir(base) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }

                let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                    continue;
                };
                if name.starts_with("openai.chatgpt-") {
                    extensions.push(path);
                }
            }
        }
    }

    extensions.sort_by(|a, b| b.cmp(a));

    for extension in extensions {
        for platform_dir in codex_bundle_platform_dirs() {
            let candidate = extension
                .join("bin")
                .join(platform_dir)
                .join(codex_bundle_file_name());
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

fn codex_bundle_file_name() -> &'static str {
    if cfg!(windows) { "codex.exe" } else { "codex" }
}

fn codex_bundle_platform_dirs() -> Vec<&'static str> {
    let mut dirs = Vec::new();

    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => dirs.push("linux-x86_64"),
        ("linux", "aarch64") => dirs.push("linux-arm64"),
        ("macos", "x86_64") => dirs.push("darwin-x64"),
        ("macos", "aarch64") => dirs.push("darwin-arm64"),
        ("windows", "x86_64") => dirs.push("windows-x86_64"),
        ("windows", "aarch64") => dirs.push("windows-arm64"),
        _ => {}
    }

    for dir in [
        "linux-x86_64",
        "linux-arm64",
        "darwin-x64",
        "darwin-arm64",
        "windows-x86_64",
        "windows-arm64",
        "win32-x64",
        "win32-arm64",
    ] {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }

    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cli-agent-{name}-{stamp}"))
    }

    #[test]
    fn path_lookup_finds_binary_in_path_entries() {
        let root = unique_temp_dir("path");
        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let binary = bin_dir.join("codex");
        fs::write(&binary, b"test").unwrap();

        let resolved = find_in_path_entries("codex", &[bin_dir]).unwrap();
        assert_eq!(resolved, binary);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bundled_codex_lookup_finds_vscode_extension_binary() {
        let root = unique_temp_dir("bundle");
        let extension = root.join(".vscode/extensions/openai.chatgpt-0.4.79-win32-x64");
        let binary = extension
            .join("bin")
            .join("linux-x86_64")
            .join(codex_bundle_file_name());
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(&binary, b"test").unwrap();

        let resolved = find_bundled_codex_in_roots(&[root.clone()]).unwrap();
        assert_eq!(resolved, binary);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(windows)]
    fn bundled_codex_lookup_finds_windows_extension_binary() {
        let root = unique_temp_dir("bundle-windows");
        let extension = root.join(".vscode/extensions/openai.chatgpt-0.4.79-win32-x64");
        let binary = extension
            .join("bin")
            .join("windows-x86_64")
            .join("codex.exe");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(&binary, b"test").unwrap();

        let resolved = find_bundled_codex_in_roots(&[root.clone()]).unwrap();
        assert_eq!(resolved, binary);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_paths_are_preserved() {
        let explicit = PathBuf::from("/tmp/custom-codex");
        assert_eq!(resolve_command_path(explicit.to_str().unwrap()), explicit);
    }

    #[test]
    fn windows_style_paths_are_preserved() {
        let explicit = r"C:\Users\tester\AppData\Local\Programs\codex.exe";
        assert_eq!(resolve_command_path(explicit), PathBuf::from(explicit));
    }
}
