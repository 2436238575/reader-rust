use crate::error::error::AppError;
use crate::model::book_group::BookGroup;
use crate::storage::db::repo::BookGroupRepo;

/// 书架分组。存储是 `book_groups` 实体表（不再是 json_documents 整文档：
/// 并发建组不再互相覆盖）。
///
/// **位域约定**（docs/api/book-group.md，与阅读3.0 一致）：书籍的 `group`
/// 字段是位掩码，组 id 必须是 2 的幂。上限 31 个组（1<<0 ..= 1<<30）——
/// 前端的位运算是 32 位有符号整数，1<<31 就是负号了。
pub struct BookGroupService {
    repo: BookGroupRepo,
}

/// groupId 的最大合法值（第 31 个幂位）。
pub const MAX_GROUP_ID: i64 = 1 << 30;

/// 客户端直传的 groupId 必须是合法幂位；0 保留给「新建，服务端分配」。
/// 非法 id（比如 3）在位掩码语义下等于同时进两个组，必须挡在入口。
fn validate_group_id(group_id: i64) -> Result<(), AppError> {
    if group_id <= 0 || group_id > MAX_GROUP_ID || (group_id & (group_id - 1)) != 0 {
        return Err(AppError::BadRequest(
            "groupId 必须是 2 的幂（1, 2, 4…2^30）：书籍分组是位掩码".to_string(),
        ));
    }
    Ok(())
}

impl BookGroupService {
    pub fn new(repo: BookGroupRepo) -> Self {
        Self { repo }
    }

    pub async fn get_groups(&self, user_ns: &str) -> Result<Vec<BookGroup>, AppError> {
        self.repo.list(user_ns).await
    }

    /// 整体替换排序（前端「保存排序」的整表提交）。
    pub async fn save_groups(
        &self,
        user_ns: &str,
        groups: &Vec<BookGroup>,
    ) -> Result<(), AppError> {
        for group in groups {
            validate_group_id(group.group_id)?;
        }
        // 整表提交语义：以提交的集合为准——不在集合里的视为已删除
        //（repo.delete 会同步清掉书籍位掩码里的对应位）
        let existing = self.repo.list(user_ns).await?;
        let keep: std::collections::HashSet<i64> =
            groups.iter().map(|group| group.group_id).collect();
        for group in &existing {
            if !keep.contains(&group.group_id) {
                self.repo.delete(user_ns, group.group_id).await?;
            }
        }
        for group in groups {
            self.repo.upsert(user_ns, group).await?;
        }
        Ok(())
    }

    /// 新建或重命名：group_id=0 时分配最小未占用的幂位（位域要求 2 的幂）；
    /// 显式 id（WebDAV 恢复对齐备份）必须是合法幂位。
    pub async fn save_group(&self, user_ns: &str, mut group: BookGroup) -> Result<(), AppError> {
        if group.group_id == 0 {
            let Some(id) = self.repo.alloc_group_id(user_ns).await? else {
                return Err(AppError::BadRequest("分组已达上限（31 个）".to_string()));
            };
            group.group_id = id;
        } else {
            validate_group_id(group.group_id)?;
        }
        self.repo.upsert(user_ns, &group).await
    }

    /// 删组：组行与书籍位掩码里的对应位一起清（repo 层单事务），
    /// 空出的幂位才可安全回填。
    pub async fn delete_group(&self, user_ns: &str, group_id: i64) -> Result<(), AppError> {
        self.repo.delete(user_ns, group_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_service() -> (BookGroupService, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-group-svc-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite:{}?mode=rwc", dir.join("reader.db").display());
        let pool = crate::storage::db::init_pool(&url).await.unwrap();
        (BookGroupService::new(BookGroupRepo::new(pool)), dir)
    }

    fn group(id: i64) -> BookGroup {
        BookGroup {
            group_id: id,
            group_name: format!("组{id}"),
            order_no: 0,
        }
    }

    /// 客户端直传的 groupId 必须是合法幂位：3（0b11）会同时命中组 1/2，
    /// 是 max_id+1 时代的老 bug 形态，必须 400 挡在入口。
    #[tokio::test]
    async fn save_group_rejects_non_power_of_two_ids() {
        let (service, dir) = test_service().await;

        for bad in [3i64, 5, 6, -1, 1 << 31] {
            let err = service.save_group("u", group(bad)).await.unwrap_err();
            assert!(
                matches!(err, AppError::BadRequest(_)),
                "非法 groupId {bad} 应 400，实际 {err:?}"
            );
        }
        // 合法幂位（WebDAV 恢复对齐备份的路径）与 0（服务端分配）正常
        service.save_group("u", group(4)).await.unwrap();
        service.save_group("u", group(0)).await.unwrap();
        let ids: Vec<i64> = service
            .get_groups("u")
            .await
            .unwrap()
            .iter()
            .map(|g| g.group_id)
            .collect();
        assert_eq!(ids, vec![1, 4], "显式 4 + 分配最小空位 1（按 id 排序）");

        // 整表提交同样逐条校验
        let err = service
            .save_groups("u", &vec![group(1), group(3)])
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));

        let _ = std::fs::remove_dir_all(dir);
    }
}
