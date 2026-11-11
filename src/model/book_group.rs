use serde::{Deserialize, Serialize};

#[derive(utoipa::ToSchema, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookGroup {
    #[serde(default)]
    pub group_id: i64,
    pub group_name: String,
    #[serde(default)]
    pub order_no: i32,
}

/// 分组排序的单条提交项（`POST /reader3/saveBookGroupOrder`）。
///
/// 排序接口**只改顺序**：不带 `groupName`，因此调用方不可能借排序接口
/// 改名/建组/删组——增删改的唯一入口是 `/saveBookGroup` 与
/// `/deleteBookGroup`。旧版用整表 `Vec<BookGroup>` 覆盖，不在数组里的组
/// 会被删除并清掉书上的位，一个「上下移」的误提交就能丢掉分组。
#[derive(utoipa::ToSchema, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookGroupOrderItem {
    pub group_id: i64,
    pub order_no: i32,
}
