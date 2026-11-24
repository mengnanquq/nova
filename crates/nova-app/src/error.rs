// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 应用级错误。
//!
//! 与 `nova-cli` 的错误模型保持同样的原则：错误信息里不出现访问令牌，也不出现
//! 带查询串的完整地址（provider / images 的错误类型已经在构造时做过脱敏）。

/// `nova-desktop` 的错误。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppError {
    /// 命令行参数不合法。
    #[error("用法错误：{0}")]
    Usage(String),

    /// 运行环境不满足要求（状态目录无法确定、显式指定的服务器不存在等）。
    #[error("配置错误：{0}")]
    Config(String),

    /// 本地 SQLite 故障。
    #[error("本地数据库错误：{0}")]
    Storage(#[from] nova_storage::StorageError),

    /// 图片缓存故障。
    #[error("图片缓存错误：{0}")]
    Image(#[from] nova_images::ImageError),

    /// provider 构造失败。
    #[error("服务端错误：{0}")]
    Provider(#[from] nova_provider::ProviderError),

    /// Slint 平台错误（窗口创建、事件循环、快照）。
    #[error("界面错误：{0}")]
    Platform(#[from] slint::PlatformError),

    /// 标准 IO。
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),

    /// 无法创建 tokio runtime。
    #[error("无法创建 tokio runtime：{0}")]
    Runtime(#[source] std::io::Error),

    /// 快照写盘失败。
    #[error("无法写入快照 {path}：{source}")]
    Snapshot {
        /// 目标路径。
        path: String,
        /// 底层 IO 错误。
        #[source]
        source: std::io::Error,
    },

    /// 播放 / 捕获失败（`--play --capture` 用退出码 3）。
    #[error("播放失败：{0}")]
    Playback(String),
}

impl AppError {
    /// 进程退出码：播放验证失败是 3（与 `--snapshot`/普通错误的 1 区分）。
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Playback(_) => 3,
            _ => 1,
        }
    }

    /// 下一步该做什么；`None` 表示错误信息本身已经够用。
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Usage(_) => Some("运行 `nova-desktop --help` 查看用法"),
            Self::Config(_) | Self::Runtime(_) | Self::Io(_) => None,
            Self::Storage(_) => Some("本地缓存可删除后重建（服务端是唯一事实来源）"),
            Self::Image(_) => Some("图片缓存损坏时可删掉数据目录下的 images/"),
            Self::Provider(_) => Some("检查服务器是否在运行；未登录时先在终端运行 `nova login`"),
            Self::Platform(_) => {
                Some("确认图形会话可用；无显示环境请使用 `--snapshot` 或 `--print-view`")
            }
            Self::Snapshot { .. } => None,
            Self::Playback(_) => Some("重跑一次看是偶发（网络/首帧慢）还是稳定失败"),
        }
    }
}
