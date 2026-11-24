// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 焦点网格移动：纯函数，不碰 Slint。

/// 把一维数量按列数切成每行长度。
///
/// 例如 `chunked_rows(8, 6)` → `[6, 2]`；`columns` 为 0 时按 1 处理。
pub fn chunked_rows(len: usize, columns: usize) -> Vec<usize> {
    let columns = columns.max(1);
    let mut rows = Vec::with_capacity(len.div_ceil(columns));
    let mut remaining = len;
    while remaining > 0 {
        let take = remaining.min(columns);
        rows.push(take);
        remaining -= take;
    }
    rows
}

fn row_start(rows: &[usize], row: usize) -> usize {
    rows[..row].iter().sum()
}

/// 计算焦点移动后的扁平索引。
///
/// * `rows` 是每行条目数（空行会被跳过）。
/// * `dx` 为横向步进（-1 / 1），`dy` 为纵向步进。
/// * 到达边界时停在原地，不会绕回。
pub fn move_focus(rows: &[usize], index: usize, dx: i32, dy: i32) -> usize {
    let total: usize = rows.iter().sum();
    if total == 0 {
        return 0;
    }
    let mut row = 0;
    let mut offset = index.min(total - 1);
    while row < rows.len() && offset >= rows[row] {
        offset -= rows[row];
        row += 1;
    }
    if row >= rows.len() {
        row = rows.len() - 1;
        offset = rows[row].saturating_sub(1);
    }

    if dy != 0 {
        let target_row = if dy > 0 {
            (row + 1..rows.len()).find(|candidate| rows[*candidate] > 0)
        } else {
            (0..row).rev().find(|candidate| rows[*candidate] > 0)
        };
        return match target_row {
            Some(target_row) => row_start(rows, target_row) + offset.min(rows[target_row] - 1),
            None => row_start(rows, row) + offset,
        };
    }

    if dx != 0 {
        let target = if dx > 0 {
            offset + 1
        } else {
            offset.saturating_sub(1)
        };
        if target < rows[row] {
            return row_start(rows, row) + target;
        }
        return row_start(rows, row) + offset;
    }

    row_start(rows, row) + offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_rows_handles_empty_and_partial_rows() {
        assert!(chunked_rows(0, 6).is_empty());
        assert_eq!(chunked_rows(6, 6), vec![6]);
        assert_eq!(chunked_rows(8, 6), vec![6, 2]);
        assert_eq!(chunked_rows(14, 6), vec![6, 6, 2]);
        assert_eq!(chunked_rows(3, 0), vec![1, 1, 1]);
    }

    #[test]
    fn horizontal_moves_stay_inside_the_row() {
        let rows = [3, 2];
        assert_eq!(move_focus(&rows, 0, 1, 0), 1);
        assert_eq!(move_focus(&rows, 2, 1, 0), 2);
        assert_eq!(move_focus(&rows, 0, -1, 0), 0);
        assert_eq!(move_focus(&rows, 3, 1, 0), 4);
        assert_eq!(move_focus(&rows, 4, 1, 0), 4);
    }

    #[test]
    fn vertical_moves_keep_column_and_clamp_to_shorter_row() {
        let rows = [6, 6, 2];
        assert_eq!(move_focus(&rows, 0, 0, 1), 6);
        assert_eq!(move_focus(&rows, 4, 0, 1), 10);
        // 第二行第 5 列往下：第三行只有 2 个，夹到最后一个。
        assert_eq!(move_focus(&rows, 11, 0, 1), 13);
        assert_eq!(move_focus(&rows, 12, 0, -1), 6);
        assert_eq!(move_focus(&rows, 0, 0, -1), 0);
    }

    #[test]
    fn empty_grid_returns_zero() {
        assert_eq!(move_focus(&[], 5, 1, 0), 0);
        assert_eq!(move_focus(&[0, 0], 0, 0, 1), 0);
    }
}
