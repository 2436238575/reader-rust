//! 集成测试共享的临时目录约定。
//!
//! Windows 下测试进程还活着时 SQLite 连接池锁着 reader.db，测试结束时的
//! `remove_dir_all` 会因文件被占用静默失败（`let _ =` 吞掉），每跑一轮
//! 就在 %TEMP% 留一堆孤儿目录。约定两条：
//! - 启动时清掉同名前缀的**往期**遗留（超过 10 分钟，属它的进程早退了）；
//! - 结尾删除用 `remove_dir_retried` 重试等句柄释放，仍失败就留给下一次启动。

use std::path::Path;

/// 清掉 %TEMP% 下往期测试的遗留目录（`reader-rust-` 前缀、超过 10 分钟）。
/// 只动老目录：正在跑的并行测试进程产生的目录是新的，不会被误删。
pub fn clean_stale_temp_dirs() {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("reader-rust-")
        {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// 测试结尾的目录删除：重试几次等句柄释放；仍失败就留给下一次启动时清，
/// 不要再 `let _ =` 一次了事。
pub fn remove_dir_retried(path: impl AsRef<Path>) {
    for _ in 0..5 {
        if std::fs::remove_dir_all(path.as_ref()).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
