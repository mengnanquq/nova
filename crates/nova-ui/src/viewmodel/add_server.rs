// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! “添加服务器”对话框的视图模型：地址校验、错误分类、提交门控。
//!
//! 纯逻辑，无 I/O。密码**不进**视图模型：它只在提交瞬间经
//! `Slint 属性 → 上层回调` 传给 `nova-app`，由那边包成 `SecretString`。

use url::Url;

/// 地址不合规的种类（用于内联提示与提交门控）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressError {
    /// 空地址。
    Empty,
    /// 缺少 `http://` / `https://` 前缀。
    MissingScheme,
    /// 用了别的协议（`ftp://` 等）。
    BadScheme,
    /// 地址里带用户名/密码（`http://user:pass@host`）。
    EmbeddedCredentials,
    /// 缺少主机名（`http://` 后面什么都没有）。
    MissingHost,
}

impl AddressError {
    /// 内联提示文案（出现在地址输入框正下方）。
    pub fn hint(self) -> &'static str {
        match self {
            Self::Empty => "请输入服务器地址",
            Self::MissingScheme | Self::BadScheme => "需要 http:// 或 https:// 开头",
            Self::EmbeddedCredentials => "地址里不要带用户名/密码，请在下方填写",
            Self::MissingHost => "地址缺少主机名，例如 http://192.168.1.5:8096",
        }
    }
}

/// 校验并规范化服务器地址。
///
/// 返回的字符串已去空白、去掉末尾斜杠，可直接作为 provider 的 `base_url`。
pub fn validate_address(raw: &str) -> Result<String, AddressError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AddressError::Empty);
    }
    if !trimmed.contains("://") {
        return Err(AddressError::MissingScheme);
    }
    let url = Url::parse(trimmed).map_err(|_| scheme_error(trimmed))?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(AddressError::BadScheme),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AddressError::EmbeddedCredentials);
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(AddressError::MissingHost);
    }
    // `url` 规范化后的字符串自带一个根斜杠（`http://host/`），剥掉以免拼出 `//Items`。
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// 地址解析失败时区分“缺前缀”与“有前缀但缺主机”。
fn scheme_error(raw: &str) -> AddressError {
    let head = raw.split("://").next().unwrap_or("").to_ascii_lowercase();
    if head == "http" || head == "https" {
        AddressError::MissingHost
    } else {
        AddressError::MissingScheme
    }
}

/// 地址是否走明文 HTTP（密码会未加密传输；不阻止提交，但要警示）。
pub fn is_plaintext_address(raw: &str) -> bool {
    raw.trim().to_ascii_lowercase().starts_with("http://")
}

/// 连接失败的四类原因；文案要能指导下一步。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddServerErrorKind {
    /// 401：用户名或密码不对。
    Auth,
    /// 连接失败 / 超时。
    Connect,
    /// TLS 证书校验失败。
    Tls,
    /// 服务器类型未知 / 响应异常。
    UnknownServer,
    /// 本地校验：用户名为空。此时**不联网**，直接在错误区给文案（体检 R8-C4）。
    MissingUser,
}

impl AddServerErrorKind {
    /// 主错误文案（错误区的第一行）。
    pub fn message(self) -> &'static str {
        match self {
            Self::Auth => "用户名或密码不对",
            Self::Connect => "连不上服务器：检查地址、端口和网络",
            Self::Tls => "证书校验失败：自签名证书需要导入系统信任，不能跳过校验",
            Self::UnknownServer => "这个地址看起来不像 Emby 或 Jellyfin",
            Self::MissingUser => "请填写用户名",
        }
    }
}

/// 一次连接失败的完整信息。
///
/// `detail` 是已脱敏的补充说明（`ProviderError` 的文本不含令牌与请求地址），
/// 只用于错误区第二行；主文案由 [`AddServerErrorKind::message`] 给出。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddServerError {
    /// 错误类别。
    pub kind: AddServerErrorKind,
    /// 已脱敏的补充说明；`None` 表示只显示主文案。
    pub detail: Option<String>,
}

impl AddServerError {
    /// 新建错误（无补充说明）。
    pub fn new(kind: AddServerErrorKind) -> Self {
        Self { kind, detail: None }
    }

    /// 新建错误并附带已脱敏的补充说明。
    pub fn with_detail(kind: AddServerErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: Some(detail.into()),
        }
    }

    /// 主文案。
    pub fn message(&self) -> &'static str {
        self.kind.message()
    }
}

/// 对话框状态机：空闲（可编辑可提交）↔ 连接中（禁用重复提交）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddServerState {
    /// 空闲。
    Idle,
    /// 连接请求已发出，等待结果。
    Connecting,
}

/// “添加服务器”对话框的界面状态。
///
/// 不持有密码：密码只存在于 Slint 的输入框属性里，提交时经回调直传上层。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddServerView {
    /// 对话框是否打开。
    pub open: bool,
    /// 状态机。
    pub state: AddServerState,
    /// 服务器地址（双向绑定到输入框；最近使用记忆也回填这里）。
    pub address: String,
    /// 用户名。
    pub user: String,
    /// 错误区内容；`None` 表示没有错误。
    pub error: Option<AddServerError>,
    /// 表单下方的提示（如“系统密钥环不可用，登录状态只保留到本次会话”）。
    pub note: Option<String>,
}

impl Default for AddServerView {
    fn default() -> Self {
        Self::new()
    }
}

impl AddServerView {
    /// 新建关闭状态的空表单。
    pub fn new() -> Self {
        Self {
            open: false,
            state: AddServerState::Idle,
            address: String::new(),
            user: String::new(),
            error: None,
            note: None,
        }
    }

    /// 地址校验结果。
    pub fn address_result(&self) -> Result<String, AddressError> {
        validate_address(&self.address)
    }

    /// 内联提示；`None` 表示地址当前合法（或为空时按 Empty 给提示）。
    ///
    /// 空地址不算“错误”：只是“连接”按钮被禁用，不打扰输入。
    pub fn address_hint(&self) -> Option<&'static str> {
        match self.address_result() {
            Ok(_) => None,
            Err(AddressError::Empty) => None,
            Err(error) => Some(error.hint()),
        }
    }

    /// 地址是否为明文 HTTP（显示警示条，但不阻止提交）。
    pub fn is_plaintext(&self) -> bool {
        is_plaintext_address(&self.address)
    }

    /// 当前是否可以提交：空闲且地址合法。
    pub fn can_submit(&self) -> bool {
        self.state == AddServerState::Idle && self.address_result().is_ok()
    }

    /// 打开对话框（可选预填“最近使用”的地址与用户名）。
    pub fn open_with(mut self, address: &str, user: &str) -> Self {
        self.open = true;
        if !address.trim().is_empty() {
            self.address = address.trim().to_owned();
        }
        if !user.trim().is_empty() {
            self.user = user.trim().to_owned();
        }
        self.error = None;
        self
    }

    /// 关闭对话框：清空密码的职责在上层（Slint 属性），这里只复位状态机与错误。
    pub fn close(mut self) -> Self {
        self.open = false;
        self.state = AddServerState::Idle;
        self.error = None;
        self
    }

    /// 进入“连接中”：禁用重复提交，清掉上一次的错误。
    pub fn begin_connecting(mut self) -> Self {
        self.state = AddServerState::Connecting;
        self.error = None;
        self
    }

    /// 连接失败：回到空闲并展示错误。
    pub fn fail(mut self, error: AddServerError) -> Self {
        self.state = AddServerState::Idle;
        self.error = Some(error);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 地址校验：6 类输入 ----

    #[test]
    fn address_validation_covers_six_input_classes() {
        // 1. 空地址
        assert_eq!(validate_address("   "), Err(AddressError::Empty));
        // 2. 缺协议前缀
        assert_eq!(
            validate_address("192.168.1.5:8096"),
            Err(AddressError::MissingScheme)
        );
        // 3. 错误协议
        assert_eq!(
            validate_address("ftp://192.168.1.5"),
            Err(AddressError::BadScheme)
        );
        // 4. 地址里带凭证
        assert_eq!(
            validate_address("http://user:pass@192.168.1.5:8096"),
            Err(AddressError::EmbeddedCredentials)
        );
        // 5. 缺主机名
        assert_eq!(validate_address("http://"), Err(AddressError::MissingHost));
        // 6. 合法：http 与 https，含规范化
        assert_eq!(
            validate_address("http://192.168.1.5:8096/"),
            Ok("http://192.168.1.5:8096".to_owned())
        );
        assert_eq!(
            validate_address("  https://media.example.com/jellyfin  "),
            Ok("https://media.example.com/jellyfin".to_owned())
        );
    }

    #[test]
    fn address_hints_map_to_guidance() {
        assert_eq!(AddressError::Empty.hint(), "请输入服务器地址");
        assert_eq!(
            AddressError::MissingScheme.hint(),
            "需要 http:// 或 https:// 开头"
        );
        assert_eq!(
            AddressError::BadScheme.hint(),
            "需要 http:// 或 https:// 开头"
        );
        assert!(AddressError::MissingHost.hint().contains("主机名"));
        assert!(AddressError::EmbeddedCredentials.hint().contains("用户名"));
    }

    // ---- 错误分类 → 文案 ----

    #[test]
    fn error_kinds_map_to_actionable_messages() {
        assert_eq!(AddServerErrorKind::Auth.message(), "用户名或密码不对");
        assert_eq!(
            AddServerErrorKind::Connect.message(),
            "连不上服务器：检查地址、端口和网络"
        );
        assert_eq!(
            AddServerErrorKind::Tls.message(),
            "证书校验失败：自签名证书需要导入系统信任，不能跳过校验"
        );
        assert_eq!(
            AddServerErrorKind::UnknownServer.message(),
            "这个地址看起来不像 Emby 或 Jellyfin"
        );
        let with_detail = AddServerError::with_detail(AddServerErrorKind::Connect, "请求超时");
        assert_eq!(with_detail.message(), AddServerErrorKind::Connect.message());
        assert_eq!(with_detail.detail.as_deref(), Some("请求超时"));
    }

    // ---- 提交门控 ----

    #[test]
    fn submit_disabled_while_connecting() {
        let mut view = AddServerView::new().open_with("http://192.168.1.5:8096", "me");
        assert!(view.can_submit());
        view = view.begin_connecting();
        assert!(!view.can_submit(), "连接中必须禁用重复提交");
        // 连接中的错误被清掉；失败后恢复空闲并展示错误。
        view = view.fail(AddServerError::new(AddServerErrorKind::Auth));
        assert!(view.can_submit(), "失败回到空闲后可以重试");
        assert_eq!(
            view.error.as_ref().map(|e| e.kind),
            Some(AddServerErrorKind::Auth)
        );
    }

    #[test]
    fn submit_disabled_with_invalid_address() {
        let view = AddServerView {
            open: true,
            address: "192.168.1.5:8096".to_owned(),
            ..AddServerView::new()
        };
        assert!(!view.can_submit());
        assert_eq!(view.address_hint(), Some("需要 http:// 或 https:// 开头"));
        // 空地址没有内联提示，但同样不可提交。
        let empty = AddServerView::new().open_with("", "");
        assert!(!empty.can_submit());
        assert_eq!(empty.address_hint(), None);
    }

    // ---- http 警示条 ----

    #[test]
    fn plaintext_warning_tracks_scheme_only() {
        let mut view = AddServerView::new().open_with("http://192.168.1.5:8096", "");
        assert!(view.is_plaintext());
        view.address = "https://192.168.1.5:8096".to_owned();
        assert!(!view.is_plaintext());
        // 大小写不敏感。
        view.address = "HTTP://192.168.1.5".to_owned();
        assert!(view.is_plaintext());
    }

    #[test]
    fn open_prefills_and_clears_error() {
        let closed = AddServerView::new().fail(AddServerError::new(AddServerErrorKind::Connect));
        let opened = closed.open_with("http://a:8096", "bob");
        assert!(opened.open);
        assert_eq!(opened.address, "http://a:8096");
        assert_eq!(opened.user, "bob");
        assert!(opened.error.is_none());
        // 关闭复位状态机，但保留最近使用的地址与用户名。
        let closed_again = opened.begin_connecting().close();
        assert!(!closed_again.open);
        assert_eq!(closed_again.state, AddServerState::Idle);
        assert_eq!(closed_again.address, "http://a:8096");
    }
}
