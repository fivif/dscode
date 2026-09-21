//! End-to-end checks for the terminal-encoding bug reported from real use.
//!
//! These drive the real tool (`DoBash`) through its real subprocess/ConPTY path
//! and assert on what the model would actually receive. The unit tests in
//! `tools/bash.rs` pin the decoding cascade in isolation; this file exists to
//! catch the case where the pieces are each correct but the wiring is not.
//!
//! ## What the investigation actually found
//!
//! On Windows every `do_bash` runs through a **ConPTY**, and the ConPTY
//! transcodes what the child writes into Unicode before handing it back. Two
//! consequences, both measured rather than assumed:
//!
//! - A child's ordinary output arrives as **valid UTF-8** — PowerShell's
//!   `中文测试` reached the tool intact with no prefix and no GBK fallback.
//!   `String::from_utf8_lossy` was therefore *not* mangling it.
//! - Bytes that are not valid in the console's code page are replaced by the
//!   ConPTY with `U+25A1` (□) **before** Rust sees them, so the GBK fallback in
//!   `decode_output` cannot recover them — it stays as a safety net for the
//!   non-ConPTY paths, not as the fix for the reported symptom.
//!
//! The symptom the user saw has two real causes, and they are fixed elsewhere:
//! our own process writing UTF-8 to a CP936 console (see
//! `dscode_core::platform::enable_utf8_console`), and the PowerShell fallback
//! emitting CP936 (see the UTF-8 prefix in `shell_command`). What this file
//! guards is the plumbing in between: that Chinese survives the pipe, and that
//! a line the decoder chokes on no longer truncates the rest of the output.

#![cfg(windows)]

use std::sync::Arc;

use dscode_core::safety::guard::SafetyGuard;
use dscode_core::tools::bash::DoBash;
use dscode_core::tools::trait_def::{Tool, ToolContext};

async fn run(command: &str) -> String {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = ToolContext::simple(
        std::env::temp_dir(),
        "test-session",
        "call-1",
        tx,
        Arc::new(SafetyGuard::new(&[], false)),
    );
    let res = DoBash::new()
        .execute(
            serde_json::json!({ "command": command, "description": "encoding check" }),
            &ctx,
        )
        .await
        .expect("do_bash returned Err");
    assert!(res.success, "command failed: {}", res.output);
    res.output
}

/// The user-facing guarantee: Chinese a command prints reaches the model as
/// Chinese, with no replacement characters.
#[tokio::test]
async fn chinese_output_survives_the_pipe() {
    let out = run("echo '中文测试'").await;
    assert!(
        out.contains("中文测试"),
        "Chinese output did not survive the pipe: {out:?}"
    );
    assert!(
        !out.contains('\u{FFFD}'),
        "replacement characters in output: {out:?}"
    );
}

/// The same guarantee for the shell the tool actually falls back to on Windows
/// machines without Git for Windows. This invokes `powershell.exe` as the child
/// — the shape `shell_command` produces — so a regression in how its output is
/// read is caught even on a machine that normally routes to bash.
#[tokio::test]
async fn powershell_chinese_output_survives_the_pipe() {
    let out = run("powershell.exe -NoProfile -NonInteractive -Command \"Write-Output '中文测试'\"")
        .await;
    assert!(
        out.contains("中文测试"),
        "PowerShell Chinese output did not survive: {out:?}"
    );
}

/// The escape sequences are gone — the other half of what the user reported.
///
/// Introducing the ConPTY (2026-08-20) made every `do_bash` result begin with
/// the pty's handshake (`ESC[?9001h`, `ESC[?1004h`, `ESC[2J`, …) and carry
/// PowerShell's console-title write and cursor restores. 1090 tool messages in
/// the reporting user's own session DB (2026-08-20 → 2026-09-11) still hold
/// them verbatim; the report was "终端执行会有奇怪的符号返回".
///
/// The unit test in `tools/bash.rs` pins `strip_ansi` against those exact bytes.
/// This one pins the wiring, because for ten days the function did not exist and
/// nothing in the suite noticed. `trim_end` rather than equality: the pty may
/// emit either LF or CRLF, and both normalise to the same visible line.
#[tokio::test]
async fn conpty_and_colour_escapes_never_reach_the_model() {
    // Nothing but the text: this is the strongest form of the claim, and it is
    // what fails first if the pty preamble ever escapes the strip again.
    let out = run("echo hi").await;
    assert_eq!(
        out.trim_end(),
        "hi",
        "the pty handshake leaked into the result: {out:?}"
    );

    // A program colouring its output, the way vite/npm/pytest/cargo do.
    let out = run("printf '\\033[32mgreen\\033[0m plain\\n'").await;
    assert!(
        !out.contains('\u{1b}'),
        "escape sequence reached the model: {out:?}"
    );
    assert!(
        out.contains("green plain"),
        "stripping took the text with it: {out:?}"
    );
}

/// The half of the bug that silently ate output: the old read loop was
/// `while let Ok(Some(line)) = reader.next_line().await`, which **ended the
/// loop** on the first invalid-UTF-8 line. One bad line truncated everything
/// after it, with no error and no indication anything was missing.
#[tokio::test]
async fn output_after_a_non_utf8_line_is_still_delivered() {
    // `printf` writes the raw bytes straight to the pipe, so the reader meets a
    // line it cannot decode as UTF-8 no matter what the console would have done
    // with them.
    let out =
        run("printf 'START\\n'; printf '\\xd6\\xd0\\xce\\xc4\\n'; printf 'END-MARKER\\n'").await;
    assert!(out.contains("START"), "first line lost: {out:?}");
    assert!(
        out.contains("END-MARKER"),
        "output after the non-UTF-8 line was dropped — the read loop broke early: {out:?}"
    );
}

/// The other half of the reported bug, and the one that actually causes 乱码:
/// a zh-CN console defaults to CP936 while the CLI and TUI write UTF-8.
/// `enable_utf8_console` is what the front-ends call at startup.
///
/// The console is forced back to CP936 first, on purpose: asserting
/// "CP is 65001 after the call" on a console that was *already* 65001 proves
/// nothing, and that is exactly how this test passed the first time it was run.
/// Forcing the failure case makes it falsifiable.
///
/// Skips honestly when the process has no console attached (a CI runner or a
/// detached harness) — there `SetConsoleOutputCP` does not stick at all.
#[test]
fn utf8_console_switch_actually_moves_the_code_page() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleOutputCP() -> u32;
        fn GetConsoleCP() -> u32;
        fn SetConsoleOutputCP(code_page: u32) -> i32;
        fn SetConsoleCP(code_page: u32) -> i32;
    }

    // SAFETY: four kernel32 exports taking/returning plain integers.
    unsafe {
        if GetConsoleOutputCP() == 0 {
            eprintln!("no console attached to this process — nothing to verify, skipping");
            return;
        }
        // Simulate the zh-CN console the bug was reported on.
        SetConsoleOutputCP(936);
        SetConsoleCP(936);
        if GetConsoleOutputCP() != 936 {
            eprintln!("console code page is not settable here — skipping");
            return;
        }
    }

    dscode_core::platform::enable_utf8_console();

    let after_out = unsafe { GetConsoleOutputCP() };
    let after_in = unsafe { GetConsoleCP() };
    assert_eq!(
        after_out, 65001,
        "console output code page did not switch away from 936"
    );
    assert_eq!(
        after_in, 65001,
        "console input code page did not switch away from 936"
    );
}

/// Real-network check for the second reported bug: Bing results polluted with
/// unrelated content.
///
/// The unit tests in `tools/web.rs` parse synthetic SERP HTML, which cannot tell
/// us whether the parser still matches **today's** actual Bing markup. This one
/// hits the live endpoint. It is `#[ignore]`d so the normal suite stays offline;
/// run it deliberately:
///
/// ```text
/// cargo test -p dscode-core --test user_reported_bugs -- --ignored --nocapture live_bing
/// ```
#[tokio::test]
#[ignore = "hits the live Bing endpoint; run deliberately"]
async fn live_bing_search_is_not_polluted() {
    use dscode_core::tools::web::DoWebSearch;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    // Drain progress events so the sender never blocks.
    tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let ctx = ToolContext::simple(
        std::env::temp_dir(),
        "test-session",
        "call-search",
        tx,
        Arc::new(SafetyGuard::new(&[], false)),
    );

    // A query whose results must be Chinese-language results about Rust.
    let res = DoWebSearch::new()
        .execute(
            serde_json::json!({ "query": "Rust 所有权 借用检查", "limit": 5 }),
            &ctx,
        )
        .await
        .expect("do_web_search returned Err");

    println!("\n===== live Bing output =====\n{}\n============================\n", res.output);
    assert!(res.success, "search reported failure: {}", res.output);

    // The pollution symptom: the tail of the page (FAQ, "related searches",
    // footer) being parsed as if it were an organic result.
    for junk in [
        "related searches",
        "相关搜索",
        "People also ask",
        "还有人搜",
        "Sign in",
        "登录",
        "Microsoft Rewards",
    ] {
        assert!(
            !res.output.to_lowercase().contains(&junk.to_lowercase()),
            "SERP boilerplate leaked into the results: {junk:?}\n{}",
            res.output
        );
    }

    // Undecoded HTML entities must never reach the model. Bing separates a
    // result's date from its snippet with `&ensp;·&ensp;`, and on the first live
    // run every dated result came back containing the literal text `&ensp;` —
    // something no synthetic-SERP unit test could have caught, because none of
    // them emitted that entity.
    for ent in [
        "&ensp;", "&emsp;", "&thinsp;", "&nbsp;", "&middot;", "&hellip;", "&mdash;",
        "&amp;", "&quot;", "&#39;",
    ] {
        assert!(
            !res.output.contains(ent),
            "raw HTML entity {ent:?} reached the model\n{}",
            res.output
        );
    }
}
