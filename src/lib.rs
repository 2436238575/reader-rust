// v1 的错误类型 Problem 含多个 String（detail / traceId 等，冷路径），体积超过
// result_large_err 的 128 字节阈值；Box 化会让全部 v1 handler 签名与 ? 传播变噪，
// 这里 crate 级豁免（/reader3 的 AppError 不受影响，仍然紧凑）。
#![allow(clippy::result_large_err)]

pub mod api;
pub mod app;
pub mod auth;
pub mod crawler;
pub mod error;
pub mod model;
pub mod parser;
pub mod service;
pub mod storage;
pub mod util;
