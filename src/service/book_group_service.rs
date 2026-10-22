use crate::error::error::AppError;
use crate::model::book_group::BookGroup;
use crate::storage::db::repo::BookGroupRepo;

/// 书架分组。存储是 `book_groups` 实体表（不再是 json_documents 整文档：
/// 并发建组不再互相覆盖）。
///
/// **位域约定**（docs/api/book-group.md，与阅读3.0 一致）：书籍的 `group`
/// 字段是位掩码，组 id 必须是 2 的幂（1, 2, 4, 8…），最多 63 个组。
pub struct BookGroupService {
    repo: BookGroupRepo,
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
        // 整表提交语义：以提交的集合为准——不在集合里的视为已删除
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

    /// 新建或重命名：group_id=0 时分配最小未占用的幂位（位域要求 2 的幂）。
    pub async fn save_group(&self, user_ns: &str, mut group: BookGroup) -> Result<(), AppError> {
        if group.group_id == 0 {
            let Some(id) = self.repo.alloc_group_id(user_ns).await? else {
                return Err(AppError::BadRequest("分组已达上限（63 个）".to_string()));
            };
            group.group_id = id;
        }
        self.repo.upsert(user_ns, &group).await
    }

    pub async fn delete_group(&self, user_ns: &str, group_id: i64) -> Result<(), AppError> {
        self.repo.delete(user_ns, group_id).await
    }
}
