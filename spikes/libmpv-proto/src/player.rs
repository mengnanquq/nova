// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 一个“headless libmpv 会话”：mpv core + software 渲染上下文 + 首帧就绪检测。
//!
//! 关键的 headless 配置（全部在 `mpv_initialize()` 之前设置）：
//! - `vo=libmpv`：由 render API 驱动的 VO，不创建任何窗口；
//! - `config=no` / `load-scripts=no`：不受用户 `mpv.conf` 与脚本污染；
//! - `audio=no`：本机没有音频服务器，出声音只会污染测量；
//! - `untimed=yes` + `video-sync=no`：不按显示时钟节流，用于测吞吐上限。

use std::path::Path;
use std::time::{Duration, Instant};

use crate::sys::{AlignedImage, Event, Handle, SurfaceFormat, SwRenderer};

/// 打开会话时的参数。
pub struct SessionOptions<'a> {
    /// `--hwdec` 的值，默认 `auto-safe`（与播放器默认一致）。
    pub hwdec: &'a str,
    /// 额外的 mpv option（初始化前设置）。
    pub extra: &'a [(&'a str, &'a str)],
}

impl Default for SessionOptions<'_> {
    fn default() -> Self {
        Self {
            hwdec: "auto-safe",
            extra: &[],
        }
    }
}

/// 字段顺序有意义：Drop 顺序 = 声明顺序。
/// render context 必须在 mpv core 销毁之前释放（render.h 的硬性要求），
/// 所以 `renderer` 声明在 `player` 之前。
pub struct Session {
    renderer: SwRenderer,
    player: Handle,
}

impl Session {
    pub fn open(file: &Path, opts: &SessionOptions<'_>) -> Result<Self, String> {
        let player = Handle::create()?;
        for (name, value) in [
            ("config", "no"),
            ("load-scripts", "no"),
            ("terminal", "no"),
            ("idle", "yes"),
            ("keep-open", "yes"),
            ("audio", "no"),
            ("vo", "libmpv"),
            ("hwdec", opts.hwdec),
            ("untimed", "yes"),
            ("video-sync", "desync"),
        ] {
            player.set_option(name, value)?;
        }
        for (name, value) in opts.extra {
            player.set_option(name, value)?;
        }
        player.initialize()?;
        // render context 必须在第一个 VO 创建之前建好，否则视频初始化会失败
        // 或退回到自己开窗的 VO（render.h "Context and handle lifecycle"）。
        let renderer = SwRenderer::create(&player)?;
        let path = file.to_string_lossy().into_owned();
        player.command(&["loadfile", &path, "replace"])?;
        Ok(Self { renderer, player })
    }

    pub fn player(&self) -> &Handle {
        &self.player
    }

    pub fn renderer(&self) -> &SwRenderer {
        &self.renderer
    }

    /// 等待 `file-loaded`；遇到 `end-file` 且带错误则返回错误。
    pub fn wait_loaded(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.player.next_event(0.1) {
                Some(Event::FileLoaded) => return Ok(()),
                // `idle=yes` 在加载开始前会先发一个 idle 事件，不能当成失败。
                Some(Event::EndFile { error: 0 }) => {
                    return Err("文件在 file-loaded 之前就结束了".to_string());
                }
                Some(Event::EndFile { error }) => {
                    return Err(format!("打开文件失败: {}", crate::sys::error_string(error)));
                }
                _ => {}
            }
        }
        Err(format!("等待 file-loaded 超时（{timeout:?}）"))
    }

    /// 排空事件队列；返回 `true` 表示播放已正常结束（EOF）。
    pub fn drain_events(&self) -> Result<bool, String> {
        let mut ended = false;
        while let Some(ev) = self.player.next_event(0.0) {
            match ev {
                Event::EndFile { error: 0 } => ended = true,
                Event::EndFile { error } => {
                    return Err(format!("播放中断: {}", crate::sys::error_string(error)));
                }
                // idle=yes 下 EOF 之后还会来一个 idle。
                Event::Idle => ended = true,
                _ => {}
            }
        }
        Ok(ended)
    }

    /// 解码后的视频尺寸（像素），视频还没初始化时为 `None`。
    pub fn video_size(&self) -> Option<(u32, u32)> {
        let w = self.player.get_i64("width")?;
        let h = self.player.get_i64("height")?;
        if w <= 0 || h <= 0 {
            return None;
        }
        Some((w as u32, h as u32))
    }

    /// 等第一帧就绪，按视频原始尺寸分配缓冲区并渲染一次，返回该缓冲区。
    pub fn first_frame(
        &self,
        format: SurfaceFormat,
        timeout: Duration,
    ) -> Result<AlignedImage, String> {
        self.first_frame_with_size(format, timeout, None)
    }

    /// 同上，但可以指定渲染目标尺寸（mpv 会按显示规则缩放并加黑边）。
    pub fn first_frame_with_size(
        &self,
        format: SurfaceFormat,
        timeout: Duration,
        size: Option<(u32, u32)>,
    ) -> Result<AlignedImage, String> {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain_events()?;
            if SwRenderer::has_new_frame(self.renderer.update()) {
                if let Some((w, h)) = size.or_else(|| self.video_size()) {
                    let mut img = AlignedImage::new(w, h);
                    self.renderer.render_into(format, &mut img)?;
                    return Ok(img);
                }
            }
            if Instant::now() >= deadline {
                return Err(format!("等待首帧超时（{timeout:?}）"));
            }
            // 没帧就等更新回调唤醒，顺便让出 CPU。
            self.renderer.signal().wait(Duration::from_millis(20));
        }
    }

    /// 当前实际生效的硬件解码方式（软解时为 `no`）。
    pub fn hwdec_current(&self) -> Option<String> {
        self.player.get_string("hwdec-current")
    }

    /// 从现在起暂停播放（用于隔离“纯渲染”成本）。
    pub fn pause(&self) -> Result<(), String> {
        self.player.set_property_string("pause", "yes")
    }
}
