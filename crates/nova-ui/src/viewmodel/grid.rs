// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 网格窗口化：把"滚动偏移 → 实例化的卡片区间"抽成一个纯函数。
//!
//! # 为什么需要
//!
//! `PosterGrid` 以前是 `for card in cards`：模型里有几张卡就实例化几张。4546 张的
//! 海外电影库意味着 4546 个 `PosterCard`（各自带海报 `Image`、两段 `Text`、
//! `TouchArea`、目标登记绑定）一次性建好，实测约 27KB/张：865 卡库与 4546 卡库在
//! 同一进程、同一操作序列下的 RSS 差 ≈ 99MB，滚轮滚动时 UI 线程 CPU ≈ 11.5%。
//!
//! 现在网格只实例化"可见行 ± 缓冲行"（1280×800 下 42 张），但**内容总高度仍按
//! 全部行算**（滚动条、`scroll-to-index`、键盘焦点导航、`target-id` 编号都保持
//! 全局下标语义）。
//!
//! # 口径
//!
//! 输入是 `.slint` 侧的真实几何（不在这里重算布局公式）：
//! - `scroll_y`：视口上沿在**内容坐标**里的位置，0 = 内容顶部（= `-Flickable.content-y`）；
//! - `viewport_h`：视口高度（`Flickable.height`）；
//! - `grid_top`：内容区顶部到第一行卡片上沿的距离（媒体库是标题行高）；
//! - `row_height`：行高（卡片总高 + 行间距）；
//! - `columns`：列数；`total`：卡片总数。
//!
//! 输出是按行对齐的全局下标区间 `[start, start + count)`。
//!
//! # 退化（宁可慢，不留空洞）
//!
//! 几何还不可用（`viewport_h` / `row_height` 为 0、窗口刚建还没布局）、或者数值不
//! 是有限值（除零、脏偏移）时，退化为"从第 0 行开始的固定几行"（见
//! [`FALLBACK_ROWS`]）。这时候还没布局、一帧都画不出来，所以不会出现空洞；
//! 布局一完成就会通过 `viewport-changed` 重算真实窗口。
//!
//! （不能用"全部实例化"当退化：窗口刚建好但高度还是 0 的那一拍会让 4546 张卡
//! 又全部实例化一次，正好抵消了这次修复。）

/// 视口上下各多实例化的行数。
///
/// 缓冲行的作用是让滚轮/拖拽滚动过程中新进视口的行**已经存在**（不会先看到空行
/// 再补内容）。2 行 ≈ 半个到一个视口高度，1440p 下最多多实例化 12 张卡，代价可忽略。
pub const BUFFER_ROWS: usize = 2;

/// 几何还不可用时的退化行数（从第 0 行开始）。
///
/// 比任何真实视口都矮（1280×800 约 3 行可见），但它只在"还没布局"的那一拍生效：
/// 一次性把 4546 张全建出来比这里多两个数量级。
pub const FALLBACK_ROWS: usize = 6;

/// 需要实例化的卡片区间（全局下标，按行对齐）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GridWindow {
    /// 首个实例化卡片的全局下标（`0 <= start <= total`）。
    pub start: usize,
    /// 实例化张数（`start + count <= total`）。
    pub count: usize,
}

impl GridWindow {
    /// 从第 0 行开始的固定几行（几何未知时的退化，见 [`FALLBACK_ROWS`]）。
    pub fn lead(columns: usize, total: usize) -> Self {
        let columns = columns.max(1);
        Self {
            start: 0,
            count: total.min(columns.saturating_mul(FALLBACK_ROWS)),
        }
    }
}

/// 计算需要实例化的卡片区间。
///
/// `total` 为 0（空库 / 空搜索结果）时返回空区间。
pub fn grid_window(
    scroll_y: f64,
    viewport_h: f64,
    grid_top: f64,
    row_height: f64,
    columns: usize,
    total: usize,
) -> GridWindow {
    if total == 0 || columns == 0 {
        return GridWindow::default();
    }
    // 几何不可用 / 退化取值（还没布局、行高为 0、脏 NaN）：退化为"顶部固定几行"。
    // （`is_finite` 同时挡住 NaN / Inf；随后的 `<= 0.0` 只需处理"没布局"与
    //  "行高还没算出来"这两种正常退化。）
    let geometry_ready = scroll_y.is_finite()
        && viewport_h.is_finite()
        && grid_top.is_finite()
        && row_height.is_finite();
    if !geometry_ready || viewport_h <= 0.0 || row_height <= 0.0 {
        return GridWindow::lead(columns, total);
    }
    let columns = columns.max(1);
    let total_rows = total.div_ceil(columns);
    // 视口上沿在内容坐标里不会小于 0（`-content-y`，顶到 0 就钳住）。
    let view_top = scroll_y.max(0.0);
    // 行 `r` 占 `[grid_top + r*row_height, grid_top + (r+1)*row_height)`：
    // 第一行可见 = 上沿落在视口下沿之上的那些行里最小的一个。
    let first_visible = ((view_top - grid_top) / row_height).floor().max(0.0);
    let last_visible = ((view_top + viewport_h - grid_top) / row_height)
        .ceil()
        .max(0.0);
    // `as usize` 之前先夹到行数范围：脏偏移（切库时 `content-y` 还没被重新钳住）
    // 不该产生越界下标。
    let first_visible = (first_visible as usize).min(total_rows.saturating_sub(1));
    let last_visible = (last_visible as usize).min(total_rows);

    let first_row = first_visible.saturating_sub(BUFFER_ROWS);
    let last_row = (last_visible + BUFFER_ROWS).min(total_rows);
    // 视口比内容还高 / 滚到底时 `last_row` 也覆盖不到最后一行的情况：
    // 保证至少有一行，避免"网格整块空白、但滚动条还有内容"。
    let last_row = last_row.max((first_row + 1).min(total_rows));

    let start = first_row * columns;
    let end = (last_row * columns).min(total);
    GridWindow {
        start: start.min(total),
        count: end.saturating_sub(start),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1280 宽的媒体库：6 列、卡宽 136.7、卡片总高 223.3、行高 235.3。
    const ROW_H: f64 = 235.3;
    /// 媒体库页的网格起点（顶栏 68 + 24 + 32 + 16）。
    const GRID_TOP: f64 = 140.0;
    /// 1280×800 窗口的网格视口高。
    const VIEWPORT: f64 = 800.0;
    /// 海外电影库的卡片数。
    const OVERSEAS: usize = 4546;

    fn window(scroll_y: f64, viewport_h: f64, columns: usize, total: usize) -> GridWindow {
        grid_window(scroll_y, viewport_h, GRID_TOP, ROW_H, columns, total)
    }

    #[test]
    fn top_of_the_grid_instantiates_a_bounded_window() {
        let w = window(0.0, VIEWPORT, 6, OVERSEAS);
        assert_eq!(w.start, 0, "顶部从第 0 张开始");
        // (800 - 140)/235.3 = 2.8 → 3 行可见（第 2 行只露出下半截也算）+ 下方 2 行缓冲
        // （上方没有）→ 5 行 × 6 列。
        assert_eq!(w.count, 5 * 6);
        assert!(
            w.count < OVERSEAS / 60,
            "顶部窗口必须远小于全量（{} vs {OVERSEAS}）",
            w.count
        );
    }

    #[test]
    fn window_follows_the_scroll_offset() {
        // 滚到第 100 行（内容坐标 140 + 100×235.3）。
        let w = window(GRID_TOP + 100.0 * ROW_H, VIEWPORT, 6, OVERSEAS);
        // 第 100 行可见 → 98 行开始（缓冲 2 行），到 104 行前（可见到 103 行 + 2）。
        assert_eq!(w.start, 98 * 6);
        assert_eq!(w.count % 6, 0, "窗口按行对齐");
        assert_eq!(w.count, 8 * 6);
        // 全局下标语义：窗口里的第一张就是 98×6。
        assert!(w.start <= 100 * 6);
        assert!(w.start + w.count >= 104 * 6);
    }

    #[test]
    fn bottom_of_the_grid_stops_at_the_last_card() {
        let total_rows = OVERSEAS.div_ceil(6);
        let content = GRID_TOP + total_rows as f64 * ROW_H + 32.0;
        let max_scroll = content - VIEWPORT;
        let w = window(max_scroll, VIEWPORT, 6, OVERSEAS);
        assert_eq!(
            w.start + w.count,
            OVERSEAS,
            "滚到底时窗口必须正好收在最后一张卡上（不越界、不丢尾行）"
        );
        assert!(w.start < OVERSEAS);
        assert!(w.count >= 6, "最后一行必须实例化");
        // 最后一张卡在窗口里（滚到底能点到它）。
        assert!(w.start < OVERSEAS && OVERSEAS - 1 < w.start + w.count);
    }

    #[test]
    fn viewport_taller_than_content_instantiates_everything() {
        let small = 12;
        let w = window(0.0, 5000.0, 6, small);
        assert_eq!(
            w,
            GridWindow {
                start: 0,
                count: 12
            }
        );
        // 内容只有一行时同理（不能出现空窗口）。
        let w = window(0.0, 5000.0, 6, 3);
        assert_eq!(w, GridWindow { start: 0, count: 3 });
    }

    #[test]
    fn column_change_keeps_rows_aligned_and_the_total_exact() {
        // 同一份数据（1000 张）在 6 列与 9 列下都要按行对齐、不越界。
        for columns in [1, 2, 6, 7, 9] {
            let total = 1000;
            let w = window(GRID_TOP + 40.0 * ROW_H, VIEWPORT, columns, total);
            assert_eq!(w.start % columns, 0, "{columns} 列：窗口起点必须行对齐");
            assert!(w.count % columns == 0 || w.start + w.count == total);
            assert!(w.start + w.count <= total);
            // 视口里那一行必须在窗口内。
            let row = 40;
            let first = w.start / columns;
            let last = (w.start + w.count).div_ceil(columns);
            assert!(
                first <= row && row < last,
                "{columns} 列：第 {row} 行必须在窗口内"
            );
        }
    }

    #[test]
    fn resizing_the_viewport_grows_and_shrinks_the_window() {
        let scroll = GRID_TOP + 200.0 * ROW_H;
        let small = window(scroll, 600.0, 6, OVERSEAS);
        let large = window(scroll, 1400.0, 6, OVERSEAS);
        assert!(
            large.count > small.count,
            "视口变高必须多实例化几行（{} vs {}）",
            large.count,
            small.count
        );
        assert!(large.count <= 12 * 6, "窗口仍要有上界（{}）", large.count);
        // 变矮时不能出现空窗口。
        let tiny = window(scroll, 1.0, 6, OVERSEAS);
        assert!(tiny.count >= 6, "视口再矮也要有可见行 + 缓冲行");
    }

    #[test]
    fn stale_offset_beyond_the_end_still_shows_the_last_rows() {
        // 切库时 `content-y` 还没被 Flickable 重新钳住：偏移可能超出新内容高度。
        let w = window(1_000_000.0, VIEWPORT, 6, OVERSEAS);
        assert!(w.start + w.count <= OVERSEAS);
        assert!(w.count > 0, "脏偏移不能把网格整块变空");
        assert_eq!(w.start + w.count, OVERSEAS, "夹到最后一屏");
    }

    #[test]
    fn unknown_geometry_falls_back_to_a_bounded_lead_window() {
        // 窗口还没布局（viewport 0）/ 行高还没算出来 / 脏偏移：退回"从顶部起固定几行"，
        // 而不是"全部实例化"（后者会让 4546 张卡在首帧又全部建一次）。
        let lead = GridWindow {
            start: 0,
            count: 6 * FALLBACK_ROWS,
        };
        assert_eq!(window(0.0, 0.0, 6, OVERSEAS), lead);
        assert_eq!(grid_window(0.0, VIEWPORT, GRID_TOP, 0.0, 6, OVERSEAS), lead);
        assert_eq!(
            grid_window(f64::NAN, VIEWPORT, GRID_TOP, ROW_H, 6, OVERSEAS),
            lead
        );
        // 内容比退化窗口还小：就全建（小库没有窗口化的意义）。
        assert_eq!(
            window(0.0, 0.0, 6, 12),
            GridWindow {
                start: 0,
                count: 12
            }
        );
        // 空库 / 没算列的退化：空窗口，不 panic。
        assert_eq!(window(0.0, VIEWPORT, 6, 0), GridWindow::default());
        assert_eq!(window(0.0, VIEWPORT, 0, 10), GridWindow::default());
    }

    #[test]
    fn every_visible_row_is_inside_the_window() {
        // 从顶滚到底，每一步"视口里出现的行"都必须在窗口里（否则滚动时会看到空洞）。
        let total_rows = OVERSEAS.div_ceil(6);
        let mut scroll = 0.0;
        while scroll <= GRID_TOP + total_rows as f64 * ROW_H {
            let w = window(scroll, VIEWPORT, 6, OVERSEAS);
            let first = w.start / 6;
            let last = (w.start + w.count).div_ceil(6);
            let visible_first = ((scroll - GRID_TOP) / ROW_H).floor().max(0.0) as usize;
            let visible_last =
                (((scroll + VIEWPORT - GRID_TOP) / ROW_H).ceil().max(0.0) as usize).min(total_rows);
            assert!(
                first <= visible_first && visible_last <= last,
                "scroll={scroll}: 视口行 {visible_first}..{visible_last} 必须落在窗口行 {first}..{last} 里"
            );
            scroll += ROW_H / 2.0;
        }
    }
}
