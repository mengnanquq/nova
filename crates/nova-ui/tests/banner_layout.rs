// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 横幅布局断言：横幅出现只应把内容整体下移 `Theme.banner-height`，
//! 不应改变任何元素的尺寸或相对位置。
//!
//! 做法：同一个 HomeView fixture 渲染「无横幅 / 有横幅」两版，
//! 断言有横幅版本的像素 (x, y + 48) 与无横幅版本的 (x, y) 逐像素一致
//! （窗口底部被裁掉的 48px 除外）。

mod common;

use common::{home_filled, home_image_ids, init_backend, install_posters};
use i_slint_core::graphics::SharedPixelBuffer;
use nova_ui::{App, AppWindow, BannerData, BannerLevel};
use slint::ComponentHandle;

const WIDTH: usize = 1280;
const HEIGHT: usize = 800;
const BANNER: usize = 48;

fn capture(app: &App) -> SharedPixelBuffer<i_slint_core::graphics::Rgba8Pixel> {
    app.ui().window().take_snapshot().unwrap()
}

#[test]
fn banner_shifts_content_without_resizing_anything() {
    init_backend();
    let app = App::new_with(AppWindow::new().unwrap());
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    let home = home_filled();
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);
    app.show_home();

    let base = capture(&app);

    app.set_banner(Some(
        BannerData::new("登录已过期", BannerLevel::Warning).with_action("重新登录"),
    ));
    let with = capture(&app);
    app.set_banner(None);

    let base = base.as_slice();
    let with = with.as_slice();
    let pixel = |buf: &[i_slint_core::graphics::Rgba8Pixel], x: usize, y: usize| {
        let p = &buf[y * WIDTH + x];
        (p.r, p.g, p.b, p.a)
    };

    // 统计内容区（无横幅版 y ∈ [72, 800)）里不满足“下移 48px”的像素数。
    let mut mismatched = 0usize;
    let mut first_mismatch: Option<(usize, usize)> = None;
    for y in 72..HEIGHT - BANNER {
        for x in 0..WIDTH {
            if pixel(base, x, y) != pixel(with, x, y + BANNER) {
                mismatched += 1;
                if first_mismatch.is_none() {
                    first_mismatch = Some((x, y));
                }
            }
        }
    }
    assert_eq!(
        mismatched, 0,
        "有横幅版本应等于无横幅版本整体下移 {BANNER}px；首个不一致像素 {:?}，共 {mismatched} 个",
        first_mismatch
    );

    // 横幅本身：顶部 48px 不应透明（有内容）。
    let transparent_in_banner = (0..WIDTH * BANNER).filter(|i| with[*i].a == 0).count();
    assert_eq!(transparent_in_banner, 0, "横幅区域应完全不透明");
}
