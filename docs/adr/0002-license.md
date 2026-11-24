# ADR-0002：项目许可证

- 状态：**Accepted（2026-11-16 项目所有者确认）**
- 日期：2026-11-16
- 相关：ADR-0001（播放引擎选型）

## 背景

许可证与播放引擎互为约束，必须一起定。当前 `Cargo.toml` 暂写 `GPL-3.0-or-later`，
这是在"最坏情况"下也不会返工的选择，但可能并非社区路线的最优解。

## 候选与后果

| 方案 | 优点 | 缺点 | 与候选引擎的兼容性 |
|---|---|---|---|
| MIT / Apache-2.0 | 最宽松，最容易被下游（含商业产品）采用 | 无法链接 GPL-only 代码；生态上容易被"白嫖" | 只能配 GStreamer(LGPL)，且 libmpv 必须是 `-Dgpl=false` 的 LGPL 构建 |
| MPL-2.0 | 文件级 copyleft，宽松度与保护性折中 | 生态认知度低于 GPL/MIT | 只能配 LGPL 引擎 |
| GPL-3.0-or-later | 与 GStreamer、libmpv(GPL 构建)、libVLC 全部兼容 | 分发闭源修改版需开源；部分用户/发行版顾虑 | 全兼容 |
| LGPL-2.1+（自身） | 介于两者之间 | 动态链接要求对 Rust 静态链接不友好，条款解释成本高 | 全兼容 |

## 已确认事实

- GStreamer 核心为 LGPL-2.1+，插件多为 LGPL，部分（如某些解码插件）为 GPL。
- mpv 默认构建为 GPLv2+；`-Dgpl=false` 且排除 GPL-only 源码才可视为 LGPLv2.1+。
  注意：即使这样构建，"build system 不自动构成 LGPL 授权"，需要自行确认。
- VLC 核心 LGPL-2.1+，部分模块/插件 GPL。
- Slint 采用 GPLv3 / 商业双许可（开源项目用 GPLv3 免费）。

## 建议

**GPL-3.0-or-later**。理由：

1. Slint 的开源许可本身就是 GPLv3——选 MIT 也不会改变"用了 Slint 就是 GPL 项目"这一事实，
   除非购买商业许可。
2. 与全部候选播放引擎兼容，不会在引擎定案后被迫返工。
3. 与"开放社区路线"一致：社区贡献不能被单方面闭源分发。

代价：不能被闭源商业产品直接嵌入。若未来需要，可考虑"核心库 MIT + 应用 GPL"的双轨，
但那会引入额外的边界维护成本。

## 决策

**GPL-3.0-or-later**，已由项目所有者确认。

落地要求：

1. 仓库根 `LICENSE` 为 GPL-3.0 全文；`Cargo.toml` 的 `license` 字段为 `GPL-3.0-or-later`。
2. 每个源文件首行带 `// SPDX-License-Identifier: GPL-3.0-or-later`（新增文件同样遵守，CI 不强制但 review 要看）。
3. 分发二进制时必须同时提供对应源码（GPLv3 §6），发布流程要把它写进 checklist。
4. **禁止**引入与 GPLv3 不兼容的依赖（例如仅 Apache-2.0 且带附加限制的组件、专有 SDK）。
   引入新依赖时先看许可证，这一步在 review 里做。
5. 若将来需要"核心库宽松许可 + 应用 GPL"的双轨，必须另开 ADR，且不能回头污染已有 GPL 代码。

### 与引擎选型的关系

引擎已定为 libmpv（ADR-0001），因此不再需要为许可证去自建 `-Dgpl=false` 的 LGPL 版 libmpv
——这是选 GPL 之后省下的实质成本。但仍需在分发时提供 libmpv 与 FFmpeg 的源码获取方式。
