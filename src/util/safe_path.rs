//! 路径安全工具：把用户输入收敛为安全的文件名/目录名，并做词法级的越界检查。
//!
//! 背景：`PathBuf::join` **不会**规范化 `..`，也不会阻止绝对路径替换 base。
//! 因此凡是「用户输入 → 拼路径 → 落盘/删除」的地方都必须先过这里。

use std::path::{Component, Path, PathBuf};

/// 文件名里禁止出现的字符（路径分隔符 + Windows 保留字符 + NUL）。
const FORBIDDEN_FILE_CHARS: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'];

/// Windows 保留设备名（含 `con.txt` 这类带扩展名形式）。
/// 在 Windows 上写入/删除这些名字会落到设备而非文件系统，行为异常且静默丢数据。
pub fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split(['.', ' ']).next().unwrap_or("");
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$") {
        return true;
    }
    // COM1-COM9 / LPT1-LPT9（COM0/LPT0 不是保留名）
    if upper.len() == 4 {
        if let Some(prefix) = upper.strip_suffix(|c: char| c.is_ascii_digit()) {
            let digit = upper.chars().last().unwrap_or('0');
            return matches!(prefix, "COM" | "LPT") && ('1'..='9').contains(&digit);
        }
    }
    false
}

/// 把用户提供的名字收敛为一个安全的**纯文件名**。
///
/// 拒绝：空、`.`、`..`、超过 255 字节、含控制字符、含路径分隔符或 Windows 保留字符、
///      首字符为 `.` 或 `..` 开头之外的多段路径、以 `.` 或空格结尾（Windows 会静默吞掉）。
pub fn sanitize_file_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    if name.len() > 255 {
        return None;
    }
    if name
        .chars()
        .any(|c| c.is_control() || FORBIDDEN_FILE_CHARS.contains(&c))
    {
        return None;
    }
    // 必须恰好是一个路径片段：`a/b`、`..\x`、`C:\x`、`/etc/passwd` 都会在这里被拒
    if Path::new(name).file_name().and_then(|s| s.to_str()) != Some(name) {
        return None;
    }
    // Windows 会把结尾的点和空格静默去掉，直接拒绝以免产生歧义
    if name.ends_with('.') || name.ends_with(' ') {
        return None;
    }
    if is_windows_device_name(name) {
        return None;
    }
    Some(name.to_string())
}

/// 目录名（例如上传接口的 `type` 参数）：只允许 ASCII 字母数字、下划线、连字符。
pub fn sanitize_dir_segment(raw: &str) -> Option<String> {
    let seg = raw.trim();
    if seg.is_empty() || seg.len() > 64 {
        return None;
    }
    if !seg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(seg.to_string())
}

/// 是否含 `..` 组件。
///
/// 用于「明确拒绝」而不是「静默规范化」——例如删除文件时，客户端传回来的
/// 链接本不该带回溯，出现 `..` 一律按非法处理，语义更清晰。
pub fn contains_parent_dir(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c, Component::ParentDir))
}

/// 词法规范化并把 `relative` 解析到 `base` 之下；**一旦逃出 `base` 就返回 `None`**。
///
/// 只做词法处理，不需要目标已存在（因此可用于「即将写入」的路径）。
/// 绝对路径、盘符前缀、以及越过 `base` 的 `..` 都会被拒绝。
pub fn resolve_within(base: &Path, relative: &Path) -> Option<PathBuf> {
    let mut stack: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in relative.components() {
        match comp {
            Component::Normal(part) => stack.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                // 只允许弹出普通目录，绝不允许越过 base
                if stack.pop().is_none() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if stack.is_empty() {
        // 解析结果就是 base 本身，调用方通常不希望这样
        return None;
    }
    let mut result = base.to_path_buf();
    for part in stack {
        result.push(part);
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_file_names() {
        assert!(sanitize_file_name("../../../etc/passwd").is_none());
        assert!(sanitize_file_name("..").is_none());
        assert!(sanitize_file_name(".").is_none());
        assert!(sanitize_file_name("").is_none());
        assert!(sanitize_file_name("a/b.png").is_none());
        assert!(sanitize_file_name("/etc/passwd").is_none());
        assert!(sanitize_file_name("..\\windows\\system32").is_none());
        assert!(sanitize_file_name("C:\\evil.exe").is_none());
        assert!(sanitize_file_name("name.").is_none());
        assert!(sanitize_file_name("with\0nul").is_none());
    }

    #[test]
    fn rejects_windows_device_names() {
        assert!(is_windows_device_name("CON"));
        assert!(is_windows_device_name("nul"));
        assert!(is_windows_device_name("con.txt"));
        assert!(is_windows_device_name("COM1"));
        assert!(is_windows_device_name("lpt9"));
        assert!(!is_windows_device_name("COM0"));
        assert!(!is_windows_device_name("COM10"));
        assert!(!is_windows_device_name("content.txt"));
        assert!(sanitize_file_name("NUL").is_none());
        assert!(sanitize_file_name("aux.png").is_none());
    }

    #[test]
    fn accepts_normal_file_names() {
        assert_eq!(sanitize_file_name("cover.png").as_deref(), Some("cover.png"));
        assert_eq!(
            sanitize_file_name("  中文 图片-1.jpg  ").as_deref(),
            Some("中文 图片-1.jpg")
        );
        assert_eq!(sanitize_file_name(".hidden").as_deref(), Some(".hidden"));
    }

    #[test]
    fn rejects_bad_dir_segments() {
        assert!(sanitize_dir_segment("../x").is_none());
        assert!(sanitize_dir_segment("a/b").is_none());
        assert!(sanitize_dir_segment("").is_none());
        assert!(sanitize_dir_segment("a b").is_none());
        assert!(sanitize_dir_segment(&"a".repeat(65)).is_none());
    }

    #[test]
    fn accepts_normal_dir_segments() {
        assert_eq!(sanitize_dir_segment("images").as_deref(), Some("images"));
        assert_eq!(sanitize_dir_segment("my_type-2").as_deref(), Some("my_type-2"));
    }

    #[test]
    fn resolve_within_blocks_escape() {
        let base = Path::new("/srv/storage/assets/alice");
        assert!(resolve_within(base, Path::new("../../etc/passwd")).is_none());
        assert!(resolve_within(base, Path::new("a/../../etc/passwd")).is_none());
        assert!(resolve_within(base, Path::new("/etc/passwd")).is_none());
        assert!(resolve_within(base, Path::new("")).is_none());
        assert!(resolve_within(base, Path::new(".")).is_none());
    }

    #[test]
    fn detects_parent_dir_components() {
        assert!(contains_parent_dir(Path::new("../x")));
        assert!(contains_parent_dir(Path::new("a/../b")));
        assert!(!contains_parent_dir(Path::new("a/b")));
        assert!(!contains_parent_dir(Path::new("a.b")));
    }

    #[test]
    fn resolve_within_keeps_inside() {
        let base = Path::new("/srv/storage/assets/alice");
        // 期望值用 join 构造，避免 Windows / Unix 分隔符差异
        assert_eq!(
            resolve_within(base, Path::new("images/a.png")).unwrap(),
            base.join("images").join("a.png")
        );
        // 在 base 内部来回移动是允许的
        assert_eq!(
            resolve_within(base, Path::new("images/../images/b.png")).unwrap(),
            base.join("images").join("b.png")
        );
    }
}
