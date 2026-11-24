// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 用户偏好的读写。
//!
//! 偏好整体存成一条 JSON（键 [`PREFERENCES_KEY`]），而不是一列一个字段：
//! 偏好项目前还在随原型变化，整条 JSON 让"加一个偏好"不需要迁移数据库；
//! 偏好数据量小、也不需要 SQL 过滤，拆列没有收益。
//!
//! `Duration` 字段由 serde 的默认表示（`{"secs":…,"nanos":…}`）往返，精度不丢。

use rusqlite::{OptionalExtension, params};

use nova_core::UserPreferences;

use crate::codec;
use crate::database::Database;
use crate::errors::Result;
use crate::schema::{self, PREFERENCES_KEY};

impl Database {
    /// 读用户偏好；数据库里还没有记录时返回 [`UserPreferences::default`]。
    pub fn preferences(&self) -> Result<UserPreferences> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_PREFERENCES)?;
        let raw: Option<String> = stmt
            .query_row(params![PREFERENCES_KEY], |row| row.get(0))
            .optional()?;
        match raw {
            Some(json) => codec::decode("preferences", PREFERENCES_KEY, &json),
            None => Ok(UserPreferences::default()),
        }
    }

    /// 覆盖写入用户偏好（单行 upsert）。
    pub fn set_preferences(&self, preferences: &UserPreferences) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            schema::SQL_UPSERT_PREFERENCES,
            params![PREFERENCES_KEY, codec::encode(preferences)?],
        )?;
        Ok(())
    }
}
