// `error::error` 路径被全仓引用，改名影响面大，保留模块同名嵌套
#[allow(clippy::module_inception)]
pub mod error;
