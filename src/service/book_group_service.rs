use crate::error::error::AppError;
use crate::model::book_group::{BookGroup, BookGroupOrderItem};
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
pub(crate) fn validate_group_id(group_id: i64) -> Result<(), AppError> {
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

    /// 组是否存在：入组/改组前校验，防书籍位掩码里留下悬空位。
    pub async fn group_exists(&self, user_ns: &str, group_id: i64) -> Result<bool, AppError> {
        Ok(self
            .repo
            .list(user_ns)
            .await?
            .iter()
            .any(|group| group.group_id == group_id))
    }

    /// 全部组的位掩码并集（`saveBooks(restore)` 按它剥掉悬空位）。
    pub async fn group_mask(&self, user_ns: &str) -> Result<i64, AppError> {
        Ok(self
            .repo
            .list(user_ns)
            .await?
            .iter()
            .fold(0i64, |mask, group| mask | group.group_id))
    }

    /// 整体替换分组列表（整表覆盖语义，**不在集合里的分组会被删除**并清掉
    /// 书籍位掩码上的对应位）。
    ///
    /// **不再绑定 HTTP 路由**：`/saveBookGroupOrder` 已改为只排序的
    /// [`Self::reorder_groups`]。本方法保留给 WebDAV 恢复——恢复是「以备份
    /// 为准」的整表重建，删掉备份里没有的组正是期望行为。
    pub async fn save_groups(&self, user_ns: &str, groups: &[BookGroup]) -> Result<(), AppError> {
        let mut tx = self.repo.begin().await?;
        self.save_groups_on(&mut tx, user_ns, groups).await?;
        tx.commit().await?;
        Ok(())
    }

    /// 事务版整表覆盖（WebDAV 恢复的五域单事务）。
    pub async fn save_groups_on(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_ns: &str,
        groups: &[BookGroup],
    ) -> Result<(), AppError> {
        for group in groups {
            validate_group_id(group.group_id)?;
        }
        self.repo.save_groups_on(tx, user_ns, groups).await
    }

    /// 只改分组顺序（`POST /reader3/saveBookGroupOrder`）。
    ///
    /// **语义变更**：旧版是整表覆盖（`save_groups`），不在提交数组里的分组
    /// 会被 `repo.delete` 删掉，并连带清掉书籍位掩码上的对应位——一次误提交
    /// 就能丢分组。现在只 `UPDATE order_no`：
    /// - 不删组、不改名、不动书籍位掩码；
    /// - 未出现在 `items` 里的组保持原 `order_no`（不会被打乱）；
    /// - 增删改的唯一入口仍是 `/saveBookGroup` 与 `/deleteBookGroup`。
    ///
    /// 校验（全部 400，不落库）：空数组、重复 groupId、非法幂位、组不存在。
    /// 全部校验通过后在单事务里写入，避免「前几条生效、后面报错」的半截状态。
    pub async fn reorder_groups(
        &self,
        user_ns: &str,
        items: &[BookGroupOrderItem],
    ) -> Result<(), AppError> {
        if items.is_empty() {
            return Err(AppError::BadRequest("items 不能为空".to_string()));
        }
        let existing: std::collections::HashSet<i64> = self
            .repo
            .list(user_ns)
            .await?
            .iter()
            .map(|group| group.group_id)
            .collect();
        let mut seen: std::collections::HashSet<i64> =
            std::collections::HashSet::with_capacity(items.len());
        for item in items {
            validate_group_id(item.group_id)?;
            if !seen.insert(item.group_id) {
                return Err(AppError::BadRequest(format!(
                    "groupId 存在重复项: {}",
                    item.group_id
                )));
            }
            if !existing.contains(&item.group_id) {
                return Err(AppError::BadRequest(format!(
                    "分组不存在: {}",
                    item.group_id
                )));
            }
        }

        let pairs: Vec<(i64, i32)> = items
            .iter()
            .map(|item| (item.group_id, item.order_no))
            .collect();
        let mut tx = self.repo.begin().await?;
        self.repo.reorder_on(&mut tx, user_ns, &pairs).await?;
        tx.commit().await?;
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
    /// groupId 同样要过幂位校验：传 3 会清掉组 1|2 的位却留着两个组行，
    /// 传 -1 更是把全部组位一次抹掉。
    pub async fn delete_group(&self, user_ns: &str, group_id: i64) -> Result<(), AppError> {
        validate_group_id(group_id)?;
        self.repo.delete(user_ns, group_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 收尾必须先 `pool.close().await` 再删目录（Windows 文件锁，见 repo.rs 测试约定）。
    async fn test_service() -> (BookGroupService, sqlx::SqlitePool, std::path::PathBuf) {
        crate::util::test_temp::clean_stale_test_dirs();
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-group-svc-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite:{}?mode=rwc", dir.join("reader.db").display());
        let pool = crate::storage::db::init_pool(&url).await.unwrap();
        (
            BookGroupService::new(BookGroupRepo::new(pool.clone())),
            pool,
            dir,
        )
    }

    fn group(id: i64) -> BookGroup {
        BookGroup {
            group_id: id,
            group_name: format!("组{id}"),
            order_no: 0,
        }
    }

    /// 分组排序只改 order_no：不删组、不改名、未提交的组保持原顺序。
    #[tokio::test]
    async fn reorder_groups_only_updates_order_no() {
        let (service, pool, dir) = test_service().await;
        // 三个组，order_no 依次 0/1/2
        for (index, id) in [1i64, 2, 4].iter().enumerate() {
            let mut g = group(*id);
            g.order_no = index as i32;
            service.save_group("u", g).await.unwrap();
        }

        // 只提交 4 和 1（组 2 不提交）：按旧语义组 2 会被删掉
        service
            .reorder_groups(
                "u",
                &[
                    BookGroupOrderItem {
                        group_id: 4,
                        order_no: 0,
                    },
                    BookGroupOrderItem {
                        group_id: 1,
                        order_no: 1,
                    },
                ],
            )
            .await
            .unwrap();

        let groups = service.get_groups("u").await.unwrap();
        assert_eq!(groups.len(), 3, "未提交的组不得被删除");
        let order: Vec<i64> = groups.iter().map(|g| g.group_id).collect();
        // 4 → order 0，1 → order 1，组 2 保持原 order 1（按 order_no, group_id 排序）
        assert_eq!(order, vec![4, 1, 2]);
        assert_eq!(
            groups.iter().find(|g| g.group_id == 2).unwrap().order_no,
            1,
            "未提交的组保持原 order_no"
        );
        // 名称不动
        assert_eq!(
            groups.iter().find(|g| g.group_id == 4).unwrap().group_name,
            "组4"
        );

        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 排序入口的入参校验：空、重复、非法幂位、组不存在一律 400。
    #[tokio::test]
    async fn reorder_groups_rejects_invalid_input() {
        let (service, pool, dir) = test_service().await;
        service.save_group("u", group(1)).await.unwrap();

        let cases: Vec<(Vec<BookGroupOrderItem>, &str)> = vec![
            (vec![], "空数组"),
            (
                vec![
                    BookGroupOrderItem {
                        group_id: 1,
                        order_no: 0,
                    },
                    BookGroupOrderItem {
                        group_id: 1,
                        order_no: 1,
                    },
                ],
                "重复 groupId",
            ),
            (
                vec![BookGroupOrderItem {
                    group_id: 3,
                    order_no: 0,
                }],
                "非幂位",
            ),
            (
                vec![BookGroupOrderItem {
                    group_id: 8,
                    order_no: 0,
                }],
                "组不存在",
            ),
        ];
        for (items, label) in cases {
            let err = service.reorder_groups("u", &items).await.unwrap_err();
            assert!(
                matches!(err, AppError::BadRequest(_)),
                "{label} 应 400，实际 {err:?}"
            );
        }

        // 一次都没落库：order_no 仍是被拒之前的值
        assert_eq!(service.get_groups("u").await.unwrap()[0].order_no, 0);

        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 客户端直传的 groupId 必须是合法幂位：3（0b11）会同时命中组 1/2，
    /// 是 max_id+1 时代的老 bug 形态，必须 400 挡在入口。
    #[tokio::test]
    async fn save_group_rejects_non_power_of_two_ids() {
        let (service, pool, dir) = test_service().await;

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
            .save_groups("u", &[group(1), group(3)])
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));

        // 删组同样要过幂位校验：3 会清掉组 1|2 的位，-1 会抹掉全部组位
        for bad in [3i64, -1, 0] {
            let err = service.delete_group("u", bad).await.unwrap_err();
            assert!(
                matches!(err, AppError::BadRequest(_)),
                "删组非法 groupId {bad} 应 400，实际 {err:?}"
            );
        }
        service.delete_group("u", 1).await.unwrap();

        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}
