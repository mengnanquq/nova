// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 引擎启动参数与 mpv option 拼装。
//!
//! 所有 mpv 选项都在 `docs/research/player.md` 的「实现清单」里逐条对应过，**不要凭记忆
//! 加选项**：清单之外的项必须有实测依据才能进来。

use std::path::PathBuf;

/// 平台默认的硬解模式。
///
/// 注意 `*-copy` 一律不用：ADR-0001 实测 4K HEVC 10bit 下 `vaapi-copy` 比软解还慢 67%，
/// 硬解省下的时间全被 GPU→CPU 回读吃掉。
pub fn default_hwdec() -> &'static str {
    if cfg!(target_os = "linux") {
        "vaapi"
    } else if cfg!(target_os = "windows") {
        "d3d11va"
    } else if cfg!(target_os = "macos") {
        "videotoolbox"
    } else {
        "auto-safe"
    }
}

/// Linux 上 VAAPI 零拷贝要交给 mpv 的 DRM render node 候选路径。
///
/// 默认先看环境变量 `NOVA_DRM_RENDER_NODE`，再扫 `/dev/dri/renderD*`。
/// **不假设只有 renderD128**：多 GPU 机器上可能是 renderD129…，且顺序不保证。
pub fn drm_render_node_candidates() -> Vec<PathBuf> {
    if let Ok(path) = std::env::var("NOVA_DRM_RENDER_NODE") {
        if !path.is_empty() {
            return vec![PathBuf::from(path)];
        }
    }
    let mut nodes: Vec<(u32, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev/dri") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(number) = name.strip_prefix("renderD") {
                if let Ok(number) = number.parse::<u32>() {
                    nodes.push((number, entry.path()));
                }
            }
        }
    }
    // 按 node 编号排序，让「第一个能打开的」在多卡机器上也可复现。
    nodes.sort_by_key(|(number, _)| *number);
    nodes.into_iter().map(|(_, path)| path).collect()
}

/// 引擎启动参数。
///
/// 默认值就是产品配置（`docs/research/player.md` §五）；测试只需要改 `audio_output`。
#[derive(Clone, Debug)]
pub struct EngineOptions {
    hwdec: Option<String>,
    audio_output: Option<String>,
    drm_render_node: Option<PathBuf>,
    extra_mpv_options: Vec<(String, String)>,
    log_level: String,
    block_for_target_time: bool,
    volume: f32,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            hwdec: None,
            audio_output: None,
            drm_render_node: None,
            extra_mpv_options: Vec::new(),
            log_level: "warn".to_string(),
            block_for_target_time: true,
            volume: 1.0,
        }
    }
}

impl EngineOptions {
    /// 产品默认配置。
    pub fn new() -> Self {
        Self::default()
    }

    /// 覆盖硬解模式（默认按平台取 [`default_hwdec`]）。
    pub fn hwdec(mut self, mode: impl Into<String>) -> Self {
        self.hwdec = Some(mode.into());
        self
    }

    /// 音频输出驱动名。测试必须传 `"null"`（绝不出声），产品不要设。
    pub fn audio_output(mut self, name: impl Into<String>) -> Self {
        self.audio_output = Some(name.into());
        self
    }

    /// 指定 DRM render node（不设则按 [`drm_render_node_candidates`] 扫描）。
    pub fn drm_render_node(mut self, path: impl Into<PathBuf>) -> Self {
        self.drm_render_node = Some(path.into());
        self
    }

    /// 追加一条 mpv option（初始化前设置）。
    pub fn option(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_mpv_options.push((name.into(), value.into()));
        self
    }

    /// libmpv 日志级别（`no`/`error`/`warn`/`info`/`v`/`debug`）。
    pub fn log_level(mut self, level: impl Into<String>) -> Self {
        self.log_level = level.into();
        self
    }

    /// 是否让 `mpv_render_context_render` 阻塞到目标显示时间。
    ///
    /// 默认 `true`：这是 render API 下唯一正确的 A/V 同步方式（调研 §二，
    /// `--untimed` 对 render API 无效）。只有做吞吐测量时才该设 `false`。
    pub fn block_for_target_time(mut self, yes: bool) -> Self {
        self.block_for_target_time = yes;
        self
    }

    /// 起播音量（`0.0..=1.0`，超出范围会被夹住）。
    pub fn volume(mut self, volume: f32) -> Self {
        self.volume = volume;
        self
    }

    /// 请求的硬解模式（解析过平台默认值之后的实际字符串）。
    pub fn requested_hwdec(&self) -> String {
        match &self.hwdec {
            Some(mode) => mode.clone(),
            None => default_hwdec().to_string(),
        }
    }

    pub(crate) fn log_level_value(&self) -> &str {
        &self.log_level
    }

    pub(crate) fn block(&self) -> bool {
        self.block_for_target_time
    }

    pub(crate) fn start_volume(&self) -> f32 {
        if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// DRM render node 的候选顺序（显式设置优先）。
    pub(crate) fn render_node_candidates(&self) -> Vec<PathBuf> {
        match &self.drm_render_node {
            Some(path) => vec![path.clone()],
            None => drm_render_node_candidates(),
        }
    }

    /// 拼出要交给 libmpv 的 option 列表（顺序固定，便于断言与排查）。
    ///
    /// 每一条都能在 `docs/research/player.md` §五 找到出处；`fps`/`vd-lavc-*` 之类
    /// 没有实测依据的项一律不加。
    pub fn mpv_options(&self) -> Vec<(String, String)> {
        let mut options: Vec<(String, String)> = vec![
            // 不读用户配置、不跑脚本：库嵌入必须完全受控。
            ("config".into(), "no".into()),
            ("load-scripts".into(), "no".into()),
            ("terminal".into(), "no".into()),
            // 嵌入式渲染：VO 必须是 libmpv，否则 mpv 会自己开窗。
            ("vo".into(), "libmpv".into()),
            // Slint 用 OpenGL 后端，mpv 的渲染上下文必须同一条 API。
            ("gpu-api".into(), "opengl".into()),
            ("hwdec".into(), self.requested_hwdec()),
            // audio 是最鲁棒的同步方式；interpolation 在 audio 模式下无效，显式关掉。
            ("video-sync".into(), "audio".into()),
            ("interpolation".into(), "no".into()),
            // 字幕进同一条色调映射链（HDR 片源下字幕亮度不会跟画面脱节）。
            ("blend-subtitles".into(), "video".into()),
            ("sub-auto".into(), "exact".into()),
            // mpv 0.42 的合法取值只有 no/yes/scale/force/strip（`style` 是 mpv 手册里
            // 早已移除的旧值）。`yes` 就是"用 sub-text-* 覆盖 ASS 自带样式"，语义相同。
            ("sub-ass-override".into(), "yes".into()),
            // HDR → SDR：Slint 的纹理接口只吃 8bit RGBA，没有 10bit 直通路径。
            ("tone-mapping".into(), "auto".into()),
            ("dither-depth".into(), "auto".into()),
            // `audio` 是**轨道选择**选项（no/auto/序号），不是开关；"启用音频"对应 `auto`。
            ("audio".into(), "auto".into()),
            // idle + keep-open：EOF 之后保持最后一帧并等待下一条命令，不销毁 VO。
            ("idle".into(), "yes".into()),
            ("keep-open".into(), "yes".into()),
        ];
        if let Some(ao) = &self.audio_output {
            options.push(("ao".into(), ao.clone()));
        }
        options.extend(self.extra_mpv_options.iter().cloned());
        options
    }
}

/// 历史取值在 mpv 0.42 上不被接受时的等价替换。
///
/// 默认值已经改成合法值了；这个表留着是为了兼容**用户传入**的旧配置（`NOVA_MPV_OPTIONS`
/// 之类的覆盖），以及旧版本客户端写进配置文件的残留值。其它选项失败必须直接报错，
/// 不允许静默改配置。
pub(crate) fn fallback_for(name: &str, value: &str) -> Option<&'static str> {
    match (name, value) {
        ("sub-ass-override", "style") => Some("yes"),
        ("audio", "yes") => Some("auto"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of<'a>(options: &'a [(String, String)], name: &str) -> Option<&'a str> {
        options
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn default_options_match_the_research_checklist() {
        let options = EngineOptions::new().mpv_options();
        let expected = [
            ("config", "no"),
            ("load-scripts", "no"),
            ("vo", "libmpv"),
            ("gpu-api", "opengl"),
            ("hwdec", default_hwdec()),
            ("video-sync", "audio"),
            ("interpolation", "no"),
            ("blend-subtitles", "video"),
            ("sub-auto", "exact"),
            ("sub-ass-override", "yes"),
            ("tone-mapping", "auto"),
            ("dither-depth", "auto"),
            ("audio", "auto"),
            ("idle", "yes"),
            ("keep-open", "yes"),
        ];
        for (name, value) in expected {
            assert_eq!(value_of(&options, name), Some(value), "选项 {name} 不对");
        }
        // 没有要求测音时不要出现 ao=null（否则产品就没声音了）。
        assert_eq!(value_of(&options, "ao"), None);
        // copy 类硬解在 4K 下比软解还慢，配置层不得默认使用。
        assert!(!EngineOptions::new().requested_hwdec().contains("copy"));
    }

    #[test]
    fn test_options_silence_audio_and_keep_order() {
        let options = EngineOptions::new()
            .audio_output("null")
            .option("msg-level", "all=warn")
            .mpv_options();
        assert_eq!(value_of(&options, "ao"), Some("null"));
        assert_eq!(value_of(&options, "msg-level"), Some("all=warn"));
        // 追加项排在固定清单之后，ao 在固定清单之后、追加项之前。
        let ao = options.iter().position(|(k, _)| k == "ao").unwrap_or(0);
        let extra = options
            .iter()
            .position(|(k, _)| k == "msg-level")
            .unwrap_or(0);
        assert!(ao < extra);
    }

    #[test]
    fn hwdec_override_wins_over_platform_default() {
        let options = EngineOptions::new().hwdec("no").mpv_options();
        assert_eq!(value_of(&options, "hwdec"), Some("no"));
        assert_eq!(
            EngineOptions::new().hwdec("vaapi").requested_hwdec(),
            "vaapi"
        );
    }

    #[test]
    fn render_node_env_override_has_priority() {
        let options = EngineOptions::new().drm_render_node("/dev/dri/renderD129");
        assert_eq!(
            options.render_node_candidates(),
            vec![PathBuf::from("/dev/dri/renderD129")]
        );
    }

    #[test]
    fn volume_is_clamped_and_nan_safe() {
        assert_eq!(EngineOptions::new().volume(2.0).start_volume(), 1.0);
        assert_eq!(EngineOptions::new().volume(-1.0).start_volume(), 0.0);
        assert_eq!(EngineOptions::new().volume(f32::NAN).start_volume(), 1.0);
        assert_eq!(EngineOptions::new().volume(0.25).start_volume(), 0.25);
    }

    #[test]
    fn fallbacks_only_cover_verified_options() {
        assert_eq!(fallback_for("sub-ass-override", "style"), Some("yes"));
        assert_eq!(fallback_for("audio", "yes"), Some("auto"));
        assert_eq!(fallback_for("hwdec", "vaapi"), None);
        assert_eq!(fallback_for("sub-ass-override", "no"), None);
    }

    #[test]
    fn default_hwdec_is_zero_copy_on_every_platform() {
        assert!(!default_hwdec().contains("copy"));
        if cfg!(target_os = "linux") {
            assert_eq!(default_hwdec(), "vaapi");
        }
    }
}
