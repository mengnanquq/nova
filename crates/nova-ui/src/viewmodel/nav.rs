// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 导航栈。纯数据，不依赖 Slint。

/// 页面路由。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Route {
    /// 首页。
    Home,
    /// 媒体库。
    Library {
        /// 媒体库标识。
        id: String,
        /// 媒体库显示名。
        title: String,
    },
    /// 详情页。
    Detail {
        /// 条目标识。
        item_id: String,
    },
    /// 搜索页（独立结果页）。
    Search {
        /// 发起这次搜索的查询词。
        term: String,
    },
    /// 播放外壳。
    Player {
        /// 条目标识。
        item_id: String,
    },
}

/// 路由栈。栈顶是当前页面。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Navigator {
    stack: Vec<Route>,
}

impl Navigator {
    /// 空栈（没有当前页面）。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 以某个根路由建栈。
    pub fn new(root: Route) -> Self {
        Self { stack: vec![root] }
    }

    /// 当前路由；栈空时返回 `None`。
    pub fn current(&self) -> Option<&Route> {
        self.stack.last()
    }

    /// 压入一个路由。
    pub fn push(&mut self, route: Route) {
        self.stack.push(route);
    }

    /// 弹出栈顶并返回它；栈空时返回 `None`。
    pub fn pop(&mut self) -> Option<Route> {
        self.stack.pop()
    }

    /// 替换栈顶路由；栈空时等同于压栈。
    ///
    /// 用于“同一层换个目标”：换库（`open_library`）、换查询词（`open_search` /
    /// 清空输入）、换片（播放页下方挂着另一条的详情）。这些地方以前写
    /// `pop(); push(..)`，语义一样但要保证成对出现；统一走这里，栈深度不变。
    pub fn replace_top(&mut self, route: Route) {
        match self.stack.last_mut() {
            Some(top) => *top = route,
            None => self.stack.push(route),
        }
    }

    /// 返回上一级：弹出当前路由并返回新的栈顶。
    ///
    /// 只剩根路由（或空栈）时不做任何事并返回 `None`。
    pub fn back(&mut self) -> Option<&Route> {
        if self.stack.len() <= 1 {
            return None;
        }
        self.stack.pop();
        self.stack.last()
    }

    /// 清空后只保留根路由。
    pub fn reset(&mut self, root: Route) {
        self.stack.clear();
        self.stack.push(root);
    }

    /// 栈深度。
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// 栈是否为空。
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// 从底到顶的路由切片。
    pub fn stack(&self) -> &[Route] {
        &self.stack
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(id: &str) -> Route {
        Route::Detail {
            item_id: id.to_string(),
        }
    }

    #[test]
    fn push_pop_and_current() {
        let mut nav = Navigator::new(Route::Home);
        assert_eq!(nav.current(), Some(&Route::Home));
        nav.push(detail("a"));
        assert_eq!(nav.current(), Some(&detail("a")));
        assert_eq!(nav.len(), 2);
        assert_eq!(nav.pop(), Some(detail("a")));
        assert_eq!(nav.current(), Some(&Route::Home));
        assert_eq!(nav.len(), 1);
    }

    #[test]
    fn replace_top_swaps_only_the_top() {
        let mut nav = Navigator::new(Route::Home);
        nav.push(detail("a"));
        nav.replace_top(detail("b"));
        assert_eq!(nav.len(), 2, "替换栈顶不改变栈深度");
        assert_eq!(nav.current(), Some(&detail("b")));
        assert_eq!(nav.stack()[0], Route::Home);
        assert_eq!(nav.back(), Some(&Route::Home), "返回仍然只回一层");

        let mut empty = Navigator::empty();
        empty.replace_top(Route::Home);
        assert_eq!(empty.current(), Some(&Route::Home), "空栈时等同于压栈");
    }

    #[test]
    fn empty_navigator_pops_none() {
        let mut nav = Navigator::empty();
        assert_eq!(nav.pop(), None);
        assert_eq!(nav.current(), None);
        assert_eq!(nav.back(), None);
        assert!(nav.is_empty());
    }

    #[test]
    fn back_stops_at_root() {
        let mut nav = Navigator::new(Route::Home);
        nav.push(detail("a"));
        nav.push(Route::Player {
            item_id: "a".to_string(),
        });
        assert_eq!(nav.back(), Some(&detail("a")));
        assert_eq!(nav.back(), Some(&Route::Home));
        assert_eq!(nav.back(), None);
        assert_eq!(nav.current(), Some(&Route::Home));
    }

    #[test]
    fn reset_keeps_only_root() {
        let mut nav = Navigator::new(Route::Home);
        nav.push(detail("a"));
        nav.reset(Route::Library {
            id: "movies".to_string(),
            title: "电影".to_string(),
        });
        assert_eq!(nav.len(), 1);
        assert_eq!(
            nav.current(),
            Some(&Route::Library {
                id: "movies".to_string(),
                title: "电影".to_string(),
            })
        );
    }
}
