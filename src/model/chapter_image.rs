use serde::{Deserialize, Serialize};

/// 章节里的一张配图。
///
/// 两种来源汇到同一个结构：书源的配图规则（独立接口）解析出来的，
/// 以及正文 HTML 里的 `<img>`（由前端直接渲染，不走这里）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterImage {
    pub url: String,
    /// 图片下方的说明文字（番茄是「配图（画师：奈月Oo）」）。
    pub caption: String,
    /// 插入位置：正文按 `\n` 切分后的行号（从 0 开始，插在该行之前）。
    /// 取不到位置时为 `None`，前端排在章末。
    pub para_index: Option<i32>,
    /// 原始宽高，给图片占位用；站点没给就是 0，前端按未知处理。
    pub width: i32,
    pub height: i32,
}

/// 一章的配图列表。
///
/// `enabled` 说明书源有没有声明配图规则，前端据此决定要不要去请求；
/// 「这一章没有配图」是 `enabled: true` + 空列表，不是错误。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterImages {
    pub enabled: bool,
    pub images: Vec<ChapterImage>,
}

impl ChapterImages {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn new(images: Vec<ChapterImage>) -> Self {
        Self {
            enabled: true,
            images,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_has_no_images() {
        let images = ChapterImages::disabled();
        assert!(!images.enabled);
        assert!(images.images.is_empty());
    }

    #[test]
    fn enabled_without_images_is_not_an_error() {
        let images = ChapterImages::new(Vec::new());
        assert!(images.enabled);
        assert!(images.images.is_empty());
    }
}
