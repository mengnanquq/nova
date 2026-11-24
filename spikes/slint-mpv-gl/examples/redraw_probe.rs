// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 对照组：**完全不含 mpv**，只验证“Slint 窗口 + 定时 request_redraw”能否稳定重绘。
//!
//! 运行：
//! ```text
//! WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 \
//!   cargo run -p slint-mpv-gl --example redraw_probe -- 2
//! ```
//! 输出里 `renders=` 是 BeforeRendering 被调用的次数。

slint::slint! {
    #[edition = "1.19"]
    export component Probe inherits Window {
        width: 960px;
        height: 540px;
        background: black;
        in property <int> counter;
        Text {
            text: "counter {counter}";
            color: white;
            font-size: 24px;
            x: 24px;
            y: 24px;
        }
    }
}

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{GraphicsAPI, RenderingState};

fn main() {
    let seconds: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2.0);
    let ui = Probe::new().expect("窗口创建失败");
    let renders = Rc::new(Cell::new(0usize));
    let before = renders.clone();
    let after = renders.clone();
    ui.window()
        .set_rendering_notifier(move |state, api: &GraphicsAPI<'_>| match state {
            RenderingState::BeforeRendering => before.set(before.get() + 1),
            RenderingState::AfterRendering => {
                let n = after.get();
                if n <= 3 || n % 30 == 0 {
                    let _ = api;
                    eprintln!("[probe] AfterRendering #{n}");
                }
            }
            other => eprintln!("[probe] state={other:?}"),
        })
        .expect("set_rendering_notifier 失败");

    let timer = slint::Timer::default();
    let start = Instant::now();
    let counter = Rc::new(Cell::new(0));
    let tick_renders = renders.clone();
    let ui_weak = ui.as_weak();
    let tick_counter = counter.clone();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(8),
        move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let n = tick_counter.get() + 1;
            tick_counter.set(n);
            ui.set_counter(n);
            ui.window().request_redraw();
            if n % 60 == 0 {
                eprintln!(
                    "[probe] tick #{n} renders={} elapsed={:.2}",
                    tick_renders.get(),
                    start.elapsed().as_secs_f64()
                );
            }
            if start.elapsed().as_secs_f64() >= seconds {
                eprintln!("[probe] 结束时 renders={}", tick_renders.get());
                let _ = slint::quit_event_loop();
            }
        },
    );

    ui.run().expect("事件循环失败");
}
