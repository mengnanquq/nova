// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 不依赖任何服务端的命令行测试：参数、帮助、错误引导、状态目录、明文拒绝。
//!
//! 真实 Jellyfin 的流程在 `jellyfin_live.rs` 与 `cli_session.rs` 里（由环境变量门控）。

use nova_cli::{CliError, CredentialMode, Env, run_in};

fn env(dir: &std::path::Path, credentials: CredentialMode) -> Env {
    Env {
        data_dir: dir.to_path_buf(),
        credentials,
    }
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[tokio::test]
async fn help_and_version_work_without_any_state() {
    let dir = tempfile::tempdir().expect("临时目录");
    let env = env(dir.path(), CredentialMode::Memory);

    let help = run_in(&env, argv(&["--help"])).await.expect("帮助应当成功");
    assert!(help.contains("nova login --url <URL>"), "{help}");
    assert!(help.contains("不提供"), "{help}");

    let login_help = run_in(&env, argv(&["help", "login"]))
        .await
        .expect("子命令帮助应当成功");
    assert!(login_help.contains("--insecure-plaintext"), "{login_help}");
    assert!(login_help.contains("与证书校验无关"), "{login_help}");

    let items_help = run_in(&env, argv(&["help", "items"]))
        .await
        .expect("子命令帮助应当成功");
    assert!(items_help.contains("--cached"), "{items_help}");
    assert!(items_help.contains("断网可用"), "{items_help}");

    let progress_help = run_in(&env, argv(&["help", "progress"]))
        .await
        .expect("子命令帮助应当成功");
    assert!(progress_help.contains("--paused"), "{progress_help}");
    assert!(progress_help.contains("transcode"), "{progress_help}");

    let version = run_in(&env, argv(&["--version"]))
        .await
        .expect("版本应当成功");
    assert!(version.starts_with("nova "), "{version}");
}

#[tokio::test]
async fn unknown_command_is_a_usage_error_with_a_hint() {
    let dir = tempfile::tempdir().expect("临时目录");
    let env = env(dir.path(), CredentialMode::Memory);
    let error = run_in(&env, argv(&["frobnicate"]))
        .await
        .expect_err("未知命令应当报错");
    assert!(matches!(error, CliError::Usage(_)), "{error:?}");
    assert_eq!(error.exit_code(), 2);
    assert!(error.hint().is_some());
}

#[tokio::test]
async fn empty_state_dir_explains_what_to_do_next() {
    let dir = tempfile::tempdir().expect("临时目录");
    let env = env(dir.path(), CredentialMode::Memory);

    let text = run_in(&env, argv(&["servers"]))
        .await
        .expect("空状态下 servers 应当成功");
    assert!(text.contains("还没有保存任何服务器"), "{text}");
    // 内存存储要明确说清"为什么需要重新登录"。
    assert!(text.contains("重新"), "{text}");
    assert!(dir.path().join("nova.db").exists(), "应当建出数据库文件");

    let error = run_in(&env, argv(&["items", "--library", "x"]))
        .await
        .expect_err("没有服务器时 items 应当报错");
    assert!(matches!(error, CliError::NoServer), "{error:?}");
    let hint = error.hint().unwrap_or_default();
    assert!(hint.contains("nova login"), "{hint}");
}

#[tokio::test]
async fn plaintext_login_is_refused_by_default() {
    let dir = tempfile::tempdir().expect("临时目录");
    let env = env(dir.path(), CredentialMode::Memory);

    // 明文 http 默认拒绝；这个判断发生在任何网络请求之前。
    let error = run_in(
        &env,
        argv(&[
            "login",
            "--url",
            "http://127.0.0.1:18096",
            "--user",
            "nova",
            "--password-stdin",
        ]),
    )
    .await
    .expect_err("明文连接默认应当被拒绝");
    assert!(
        matches!(error, CliError::PlaintextRefused { .. }),
        "{error:?}"
    );
    let hint = error.hint().unwrap_or_default();
    assert!(hint.contains("--insecure-plaintext"), "{hint}");
    assert!(hint.contains("证书校验"), "{hint}");
    assert!(!hint.contains("跳过证书校验的选项之外"), "{hint}");
}

#[tokio::test]
async fn unsupported_scheme_is_a_config_error() {
    let dir = tempfile::tempdir().expect("临时目录");
    let env = env(dir.path(), CredentialMode::Memory);

    for url in ["file:///etc/passwd", "javascript:alert(1)", ""] {
        let error = run_in(
            &env,
            argv(&[
                "login",
                "--url",
                url,
                "--user",
                "nova",
                "--password-stdin",
                "--insecure-plaintext",
            ]),
        )
        .await
        .expect_err(&format!("{url} 应当被拒绝"));
        assert!(matches!(error, CliError::Config(_)), "{url} → {error:?}");
    }
}

#[tokio::test]
async fn missing_arguments_are_reported_before_touching_anything() {
    let dir = tempfile::tempdir().expect("临时目录");
    let env = env(dir.path(), CredentialMode::Memory);

    for args in [
        vec!["login"],
        vec!["items"],
        vec!["play"],
        vec!["progress", "abc"],
        vec!["items", "--library", "l", "--limit", "x"],
        vec!["items", "--cached"],
        vec!["progress", "abc", "--position", "1", "--method", "copy"],
    ] {
        let error = run_in(&env, argv(&args))
            .await
            .expect_err(&format!("{args:?} 应当报用法错误"));
        assert!(matches!(error, CliError::Usage(_)), "{args:?} → {error:?}");
    }
    // 一条命令都没跑成功，但数据库文件已经建出来了（`App::open` 先于参数校验之外的部分）。
    assert!(
        run_in(&env, argv(&["servers"])).await.is_ok(),
        "状态目录应当可用"
    );
}
