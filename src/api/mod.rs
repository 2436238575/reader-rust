pub mod etag;
pub mod extract;
pub mod handlers;
pub mod inbound_guard;
pub mod problem;
pub mod router;
pub mod v1;

use crate::app::config::AppConfig;
use crate::auth::AuthState;
use crate::service::{
    agent_sidecar_service::AgentSidecarService, ai_book_service::AiBookService,
    ai_model_service::AiModelService, book_group_service::BookGroupService,
    book_service::BookService, book_source_service::BookSourceService,
    chapter_content_service::ChapterContentService, image_service::ImageService,
    json_document_service::JsonDocumentService, local_epub_book::LocalEpubBookService,
    local_txt_book::LocalTxtBookService, user_service::UserService,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    /// JWT 中间件所需的签名密钥与身份查询
    pub auth: AuthState,
    pub book_service: Arc<BookService>,
    pub book_source_service: Arc<BookSourceService>,
    pub user_service: Arc<UserService>,
    pub book_group_service: Arc<BookGroupService>,
    pub local_txt_book_service: Arc<LocalTxtBookService>,
    pub local_epub_book_service: Arc<LocalEpubBookService>,
    /// 正文池、版本时间线与每用户版本选择
    pub chapter_content_service: Arc<ChapterContentService>,
    /// 章节 diff：进程内 imara-diff 计算，无外部进程依赖
    pub chapter_diff_service: Arc<crate::service::chapter_diff_service::ChapterDiffService>,
    pub json_document_service: Arc<JsonDocumentService>,
    pub bookmark_repo: crate::storage::db::repo::BookmarkRepo,
    pub replace_rule_repo: crate::storage::db::repo::ReplaceRuleRepo,
    /// 章节标题持久覆盖层（用户改名，读取时叠加在目录缓存之上）
    pub chapter_title_override_repo: crate::storage::db::repo::ChapterTitleOverrideRepo,
    pub image_service: Arc<ImageService>,
    pub ai_book_service: Arc<AiBookService>,
    pub ai_model_service: Arc<AiModelService>,
    pub agent_sidecar_service: Arc<AgentSidecarService>,
    /// 任务注册表（P2）：单进程内存，进程重启丢任务
    pub task_service: Arc<crate::service::task_service::TaskService>,
    /// SSE 一次性票据（`?ticket=`，非浏览器客户端/导出用）
    pub sse_ticket_service: Arc<crate::service::sse_ticket_service::SseTicketService>,
    /// WebDAV 备份快照/恢复（跨书架、分组、书源、书签、替换规则）
    pub webdav_backup_service: Arc<crate::service::webdav_backup_service::WebdavBackupService>,
}
