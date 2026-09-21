//! do_bash — sandboxed shell command execution with timeout and streaming progress.

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;

use tracing::warn;

use crate::agent::stream::StreamEvent;
use crate::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};

/// Commands or command patterns that are unconditionally blocked.
/// Matches are checked via `command.contains()` after normalizing whitespace.
const DANGEROUS_COMMANDS: &[&str] = &[
    "rm -rf /",
    "mkfs.",
    "dd if=",
    ":(){ :|:& };:",
    "chmod -R 777 /",
    "> /dev/sda",
    "sudo rm",
    "sudo mv",
];

const MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024; // 10MB

/// Kill an entire process tree rooted at `pid`.
///
/// Shared by `do_bash` (after the shell exits) and `TaskManager::kill` (to stop
/// a background task) — both need the same guarantee, and the platform split is
/// the same for each:
///
/// - **Unix** — signal the process group (negative pid) so children spawned by
///   `cmd &` die with the shell; TERM first so well-behaved children can exit,
///   then KILL.
/// - **Windows** — there are no POSIX process groups, so walk the tree with
///   `taskkill /T`. This must run while the parent is still alive: `/T` follows
///   parent→child links, so an orphaned grandchild (the msys process behind Git
///   Bash's `bash.exe` wrapper) is reparented the moment its parent dies and can
///   no longer be found.
///
/// No-op on non-Unix/Windows or an invalid pid.
pub(crate) fn kill_process_tree(pid: Option<u32>) {
    #[cfg(unix)]
    {
        let Some(pid) = pid.filter(|&p| p > 1) else {
            return;
        };
        let pid = pid as i32;
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        const SIGTERM: i32 = 15;
        const SIGKILL: i32 = 9;
        // TERM first so well-behaved children can exit cleanly
        let _ = unsafe { kill(-pid, SIGTERM) };
        // Brief grace, then KILL remaining (including zombies' group mates)
        std::thread::sleep(Duration::from_millis(80));
        let r = unsafe { kill(-pid, SIGKILL) };
        if r != 0 {
            // ESRCH = already gone — expected when no orphans
            tracing::debug!(pid = pid, "kill process tree finished (may already be empty)");
        }
    }
    #[cfg(windows)]
    {
        // Windows has no POSIX process groups: walk the tree via taskkill.
        if let Some(pid) = pid.filter(|&p| p > 1) {
            let pid_str = pid.to_string();
            let mut cmd = std::process::Command::new("taskkill");
            cmd.args(["/PID", pid_str.as_str(), "/T", "/F"]);
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            let _ = cmd.status();
        }
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = pid;
    }
}

/// Well-known Git for Windows install roots, probed after the PATH-derived
/// candidates. Git is not always installed under `C:\Program Files` — it may
/// live on another drive (scoop, a portable extract, a bundled git), which is
/// why the PATH is consulted first.
#[cfg(windows)]
const WINDOWS_GIT_ROOTS: &[&str] = &[
    r"C:\Program Files\Git",
    r"C:\Program Files (x86)\Git",
    r"C:\Program Files\Git\mingw64",
];

/// Locate `bash.exe` from a Git for Windows install, preferring the install
/// that owns the `git` already on PATH.
///
/// Probing `git.exe` on the PATH is what makes this work for non-standard
/// installs: `D:\...\git\cmd\git.exe` implies `D:\...\git\{bin,usr\bin}\bash.exe`.
/// Without it, such a machine silently falls through to PowerShell, where POSIX
/// commands (`cmd &`, `&&`) fail — see `shell_command`.
///
/// Cached: `shell_command` runs on every `do_bash` call, so the PATH scan and
/// its filesystem probes must happen once per process.
#[cfg(windows)]
fn find_git_bash() -> Option<std::path::PathBuf> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();

    CACHE
        .get_or_init(|| {
            let mut roots: Vec<std::path::PathBuf> = Vec::new();

            if let Some(path_var) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path_var) {
                    if !dir.join("git.exe").is_file() {
                        continue;
                    }
                    // <root>\cmd\git.exe → <root>; also cover <root>\git.exe
                    // and <root>\mingw64\bin\git.exe layouts.
                    if let Some(parent) = dir.parent() {
                        roots.push(parent.to_path_buf());
                    }
                    roots.push(dir);
                }
            }
            roots.extend(WINDOWS_GIT_ROOTS.iter().map(std::path::PathBuf::from));

            // Git for Windows ships bash at <root>\bin\bash.exe (wrapper) and
            // <root>\usr\bin\bash.exe (the real msys binary).
            const RELATIVE: &[&str] = &[r"bin\bash.exe", r"usr\bin\bash.exe"];
            for root in roots {
                for rel in RELATIVE {
                    let candidate = root.join(rel);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
            None
        })
        .clone()
}

/// Platform-appropriate shell for `do_bash` — mirrors Claude Code's Windows
/// shell selection:
/// - Unix: `bash -c <command>` (process-group killable)
/// - Windows: ① user-configured Git Bash path (`agent.git_bash_path`),
///   ② auto-detected Git for Windows `bash.exe`, ③ fall back to PowerShell
///   (`powershell.exe -NoProfile -Command`).
pub(crate) fn shell_command(command: &str) -> (std::ffi::OsString, Vec<std::ffi::OsString>) {
    #[cfg(windows)]
    {
        // 1. User-configured Git Bash path (Claude Code parity).
        if let Ok(cfg) = crate::config::settings::Config::load() {
            let configured = cfg.agent.git_bash_path.trim();
            if !configured.is_empty() && std::path::Path::new(configured).is_file() {
                return (
                    std::ffi::OsString::from(configured),
                    vec!["-c".into(), command.into()],
                );
            }
        }
        // 2. Auto-detect Git for Windows (PATH-derived first, then the
        //    well-known install roots).
        if let Some(bash) = find_git_bash() {
            return (
                std::ffi::OsString::from(bash),
                vec!["-c".into(), command.into()],
            );
        }
        // 3. Fall back to PowerShell (Claude Code's native-Windows behavior).
        //    -NonInteractive avoids prompts hanging the tool; -OutputFormat Text
        //    forces Format-* / objects to render as text (otherwise piped,
        //    non-interactive PowerShell drops formatted-object output).
        //
        //    The two encoding statements are prepended *inside* the -Command
        //    payload (not wrapped around the user's command, which would change
        //    scoping/semantics): on a zh-CN system PowerShell otherwise emits
        //    CP936 and every han character reaches `decode_output` as raw GBK.
        //    Asking the child for UTF-8 is cheaper and lossless than relying on
        //    the GBK fallback. A `;\n`-separated prefix leaves the user command
        //    as its own top-level statement, and the string is still passed as
        //    one argv element, so Windows argv quoting is unchanged.
        let mut args: Vec<std::ffi::OsString> = Vec::new();
        args.push("-NoProfile".into());
        args.push("-NonInteractive".into());
        args.push("-OutputFormat".into());
        args.push("Text".into());
        args.push("-Command".into());
        args.push(
            format!(
                "[Console]::OutputEncoding=[Text.Encoding]::UTF8;\
                 $OutputEncoding=[Text.Encoding]::UTF8;\n{command}"
            )
            .into(),
        );
        (std::ffi::OsString::from("powershell.exe"), args)
    }
    #[cfg(not(windows))]
    {
        (std::ffi::OsString::from("bash"), vec!["-c".into(), command.into()])
    }
}

/// CREATE_NO_WINDOW — prevents a console window from flashing open when a GUI
/// app (the Tauri desktop app) spawns `cmd.exe`/`bash.exe` on Windows.
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Decode raw child output bytes into text.
///
/// Windows is the reason this exists: a ConPTY hands back **raw bytes**, and a
/// Chinese-locale PowerShell/cmd emits **CP936 (GBK)**, so the previous
/// `String::from_utf8_lossy` turned every han character into U+FFFD.
///
/// The order is deliberate and load-bearing:
/// 1. strict UTF-8 — the truth on Unix, and on Windows too once the PowerShell
///    branch of `shell_command` asks the child for UTF-8;
/// 2. GBK — the Windows zh-CN default. `encoding_rs::GBK` is the gb18030
///    decoder, so GB2312/GBK/GB18030 all round-trip here;
/// 3. `from_utf8_lossy` — last resort only, never worse than the old behavior.
///
/// Decoding is per line (terminators kept) so that a single stray invalid byte
/// only corrupts its own line instead of forcing the whole buffer through the
/// GBK fallback. `\n` is a safe split point for both encodings: GBK lead bytes
/// are >= 0x81 and trail bytes >= 0x40, so 0x0A/0x0D never appear inside a
/// multi-byte sequence.
///
/// Callers must decode **before** `strip_ansi`: escape sequences are pure
/// ASCII, so decoding cannot disturb them, whereas stripping runs over a
/// `&str` and would be fed replacement characters if the two were reversed.
pub(crate) fn decode_output(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            out.push_str(&decode_line(&bytes[start..=i]));
            start = i + 1;
        }
    }
    if start < bytes.len() {
        out.push_str(&decode_line(&bytes[start..]));
    }
    out
}

/// One line of [`decode_output`] — the encoding cascade, applied per line.
fn decode_line(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let (decoded, had_errors) = encoding_rs::GBK.decode_without_bom_handling(bytes);
    if !had_errors {
        return decoded.into_owned();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Reserve `len` bytes of the `MAX_OUTPUT_BYTES` budget in a reader task.
///
/// Returns `false` once the budget is gone, latching `truncated`. The caller
/// must keep *draining* the pipe after a `false` (so the child never blocks on
/// a full pipe) but stop forwarding chunks — that is what bounds the channels.
fn reserve_output(counter: &AtomicUsize, len: usize, truncated: &AtomicBool) -> bool {
    let prev = counter.fetch_add(len, Ordering::Relaxed);
    let fits = prev.saturating_add(len) <= MAX_OUTPUT_BYTES;
    if !fits {
        truncated.store(true, Ordering::Relaxed);
    }
    fits
}

/// Strip the ANSI/VT control sequences a child process emits whenever it
/// believes it is talking to a terminal.
///
/// On Windows that is guaranteed: `do_bash` runs the child under a ConPTY, so it
/// emits cursor moves, color changes and screen clears (`ESC[2J`, `ESC[?25l`, …)
/// alongside the text. On Unix, and for the piped `do_background` children, it
/// happens whenever the program colors its output — vite, npm, pytest, cargo.
/// Correct on a terminal, pure noise everywhere this output actually goes: the
/// agent's context window and the UI's tool card. CRLF is also normalised to LF
/// so Windows output matches the Unix path.
///
/// Shared by both readers: `run_with_conpty` strips a whole result at once, and
/// `background::pipe_to_log` strips per line as the log streams — which is why
/// this is `pub(crate)` rather than local to this module.
pub(crate) fn strip_ansi(input: &str) -> String {
    /// Longest CSI body we will consume before concluding the sequence is
    /// truncated rather than merely long. Real sequences are a handful of
    /// bytes (`?9001h`, `38;5;9m`).
    const MAX_CSI_LEN: usize = 32;

    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: ESC [ … final byte in @..~ (colors, cursor moves,
                // erase-in-display, and the private ?-prefixed modes).
                //
                // The scan is bounded: `run_with_conpty` gives up on the reader
                // at a 3 s deadline, which lands mid-escape often enough that an
                // unbounded scan would eat the rest of the output (including a
                // real error message) with no marker. Past the limit we emit
                // what we consumed verbatim instead of swallowing it.
                Some('[') => {
                    let mut consumed = String::from("\u{1b}[");
                    let mut terminated = false;
                    for _ in 0..MAX_CSI_LEN {
                        let Some(next) = chars.next() else { break };
                        consumed.push(next);
                        if ('\x40'..='\x7e').contains(&next) {
                            terminated = true;
                            break;
                        }
                    }
                    if !terminated {
                        out.push_str(&consumed);
                    }
                }
                // OSC: ESC ] … terminated by BEL or ST (ESC \).
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Two-byte escapes (ESC ( B, ESC =, ESC >, …): drop both.
                _ => {}
            },
            // ConPTY reports line endings as CRLF; keep only the LF.
            '\r' if chars.peek() == Some(&'\n') => {}
            _ => out.push(c),
        }
    }

    out
}

/// Run a command through a ConPTY (pseudo-terminal) on Windows.
///
/// Plain stdout/stderr pipes miss two classes of Windows output:
/// 1. programs that write via the Console API (`WriteConsole`) — `ver`,
///    `systeminfo`, `color`, `tree`, etc.
/// 2. PowerShell's formatted-object pipeline when there is no interactive
///    console to render through `Out-Default`.
///
/// A ConPTY makes the child believe it is attached to a real terminal, so
/// those writes become readable text on the master end. This is blocking and
/// is meant to be called from `spawn_blocking`.
#[cfg(windows)]
fn run_with_conpty(
    shell: &std::ffi::OsStr,
    shell_args: &[std::ffi::OsString],
    working_dir: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<(String, Option<u32>), String> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use std::io::Read;

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 30,
            // Deliberately far wider than any real terminal. conhost hard-wraps
            // at the pty width and reports the wrap as CRLF, which `strip_ansi`
            // then turns into a genuine '\n' — a model that patched the returned
            // text would write those synthetic breaks into the file. At 512
            // columns the wrap point is past anything a command realistically
            // prints on one line.
            cols: 512,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty failed: {e}"))?;

    let mut cmd = CommandBuilder::new(shell);
    cmd.args(shell_args);
    cmd.cwd(working_dir);

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn into pty failed: {e}"))?;

    // Capture the pid now: on timeout the tree must be killed *while the parent
    // is still alive* (see `kill_process_tree`), and after `wait()` it is gone.
    let child_pid = child.process_id();

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("pty reader failed: {e}"))?;

    // Drain the merged stdout/stderr in a thread. We do not block on it here:
    // if the pty never delivers EOF (orphaned grandchild holding the pipe), the
    // read blocks forever and joining would hang do_bash. It appends into a
    // shared buffer rather than handing the bytes over at EOF, so a missing EOF
    // degrades to "partial output" instead of "no output" — the channel is then
    // only a completion signal.
    //
    // MAX_OUTPUT_BYTES is enforced *in the reader*: the buffer can never exceed
    // the cap, so a runaway command cannot OOM the process before anyone looks
    // at the length (and the old `clone()` + `from_utf8_lossy` + `strip_ansi`
    // copies are bounded too).
    let shared = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let reader_buf = std::sync::Arc::clone(&shared);
    let truncated = std::sync::Arc::new(AtomicBool::new(false));
    let reader_truncated = std::sync::Arc::clone(&truncated);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let reader_handle = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut out) = reader_buf.lock() {
                        let room = MAX_OUTPUT_BYTES.saturating_sub(out.len());
                        if room == 0 {
                            // Cap reached: keep draining so the child never
                            // blocks on a full pty, but stop growing the buffer.
                            reader_truncated.store(true, Ordering::Relaxed);
                        } else {
                            out.extend_from_slice(&buf[..n.min(room)]);
                            if n > room {
                                reader_truncated.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(());
    });

    // Poll the child so we can enforce a timeout, then reap it.
    let start = std::time::Instant::now();
    let mut exit_code: Option<u32> = None;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = Some(status.exit_code());
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            // Tree-kill *before* killing the direct child. `taskkill /T` walks
            // live parent→child links, and the msys grandchild behind Git Bash's
            // `bash.exe` wrapper is reparented the instant its parent dies —
            // after that it can no longer be found and survives holding the port
            // and the pty. Doing it after `child.kill()` would be too late.
            kill_process_tree(child_pid);
            let _ = child.kill();
            // Bounded wait for the kill to take effect; never block forever.
            for _ in 0..20 {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }

    // CRITICAL: the child has now exited. Drop the ConPTY (master/pair) so
    // ClosePseudoConsole runs and closes the output pipe's write end; only
    // then does the reader see EOF and flush the captured output.
    drop(pair);

    // Bounded flush: wait for the reader to reach EOF, then take everything it
    // captured — including when the wait times out and only a prefix arrived.
    //
    // Joining is what keeps "the pty never gave EOF" from leaking the thread,
    // its reader clone and its buffer forever: the thread sends on `tx` only
    // after its read loop ends, so a received signal means `join()` returns
    // immediately. On timeout the thread is still parked in `read()`, and the
    // JoinHandle is dropped (detached) rather than waited on forever.
    match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(()) => {
            let _ = reader_handle.join();
        }
        Err(_) => {
            warn!(
                "run_with_conpty: reader thread still blocked on the pty after 3s \
                 (orphaned grandchild holding it?); collecting partial output"
            );
        }
    }

    // `mem::take` hands the buffer over instead of cloning it a second time.
    let bytes = shared
        .lock()
        .map(|mut b| std::mem::take(&mut *b))
        .unwrap_or_default();
    let mut output = strip_ansi(&decode_output(&bytes));
    if truncated.load(Ordering::Relaxed) {
        output.push_str("\n[output truncated at 10MB]\n");
    }

    Ok((output, if timed_out { None } else { exit_code }))
}

/// Check whether a command string contains any blocked dangerous pattern.
fn is_dangerous(command: &str) -> Option<&'static str> {
    let normalized = command.trim();
    // Also collapse multiple spaces for matching
    let collapsed: String = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    for pattern in DANGEROUS_COMMANDS {
        if collapsed.contains(pattern) {
            return Some(pattern);
        }
    }
    // Check for bare "> /dev/sd" pattern even with odd spacing
    let compact: String = normalized.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains(">/dev/sd") || compact.contains(">/dev/hd") {
        return Some("> /dev/sd*");
    }
    None
}

/// The `do_bash` tool: executes a shell command in the session's working
/// directory, captures stdout/stderr, and streams output chunks back to the
/// agent loop via `ToolProgress` events.
pub struct DoBash {
    /// Default timeout for command execution in seconds.
    default_timeout_secs: u64,
}

impl DoBash {
    /// Create a new `DoBash` instance with the default timeout (120s).
    pub fn new() -> Self {
        Self {
            default_timeout_secs: 120,
        }
    }

    /// Create a new instance with a custom default timeout.
    pub fn with_timeout(secs: u64) -> Self {
        Self {
            default_timeout_secs: secs,
        }
    }
}

impl Default for DoBash {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DoBash {
    fn name(&self) -> &str {
        "do_bash"
    }

    fn description(&self) -> &str {
        "Execute a short shell command and wait for it to finish (timeout applies). \
         Good for: ls, git, tests, one-shot builds, curl checks. \
         NEVER use for long-running servers/watchers (vite, npm run dev, cargo watch) — \
         those hang this tool forever; use do_background instead, then do_task_kill to stop."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Optional timeout in seconds (default: 120). Maximum: 600."
                },
                "description": {
                    "type": "string",
                    "description": "Clear, concise description of what this command does (5-10 words)."
                }
            },
            "required": ["command", "description"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("command".into()))?;

        // T1: Validate command against dangerous patterns
        if command.trim().is_empty() {
            return Ok(ToolResult::err("", "command must not be empty"));
        }

        // Legacy substring hard list (defense in depth)
        if let Some(pattern) = is_dangerous(command) {
            return Ok(ToolResult::err(
                "",
                format!(
                    "Command blocked by safety policy: detected dangerous pattern '{}'.",
                    pattern
                ),
            ));
        }

        // Risk classification: hard block / confirm / allow
        use crate::safety::guard::CommandRisk;
        match ctx.safety_guard.classify_command(command) {
            CommandRisk::HardBlock { reason } => {
                return Ok(ToolResult::err(
                    "",
                    format!(
                        "Blocked (hard): {reason}. Never allowed, even in absolute trust mode."
                    ),
                ));
            }
            CommandRisk::Confirm { reason } if !ctx.safety_guard.absolute_trust => {
                if let Some(hub) = ctx.permission_hub.as_ref() {
                    let allowed = hub
                        .request_confirm(
                            &ctx.sender,
                            &ctx.tool_call_id,
                            command,
                            &reason,
                            ctx.permission_timeout_secs,
                        )
                        .await;
                    if !allowed {
                        return Ok(ToolResult::err(
                            "",
                            format!(
                                "User denied or timed out confirming dangerous command ({reason}): {command}"
                            ),
                        ));
                    }
                } else {
                    return Ok(ToolResult::err(
                        "",
                        format!(
                            "Dangerous command requires confirmation ({reason}) but no UI permission hub is available. \
                             Enable absolute trust or run from the desktop app. Command: {command}"
                        ),
                    ));
                }
            }
            CommandRisk::Confirm { .. } | CommandRisk::Allow => {}
        }

        let timeout_secs = args["timeout"]
            .as_u64()
            .unwrap_or(self.default_timeout_secs)
            .min(600);
        // T4: Minimum timeout of 5 seconds to prevent zero-timeout immediate failures
        let timeout_secs = timeout_secs.max(5);

        // T5: Verify working directory exists
        if !ctx.working_dir.exists() {
            return Err(ToolError::Internal(format!(
                "Working directory does not exist: {}",
                ctx.working_dir.display()
            )));
        }

        if !ctx.working_dir.is_dir() {
            return Err(ToolError::Internal(format!(
                "Working directory is not a directory: {}",
                ctx.working_dir.display()
            )));
        }

        // Emit ToolProgress with a description
        let _ = ctx.sender.send(StreamEvent::ToolProgress {
            id: ctx.tool_call_id.clone(),
            chunk: format!("$ {}\n", command),
        });

        // Build the command with process group support
        let (shell, shell_args) = shell_command(command);

        // Windows: run through a ConPTY so Console-API writes (ver/systeminfo/
        // color) and PowerShell formatted-object output are captured, not just
        // what arrives on the stdout/stderr pipes.
        #[cfg(windows)]
        {
            let working_dir = ctx.working_dir.clone();
            let shell = shell.clone();
            let shell_args = shell_args.clone();
            let timeout = Duration::from_secs(timeout_secs);
            let (output, exit_code) = tokio::task::spawn_blocking(move || {
                run_with_conpty(&shell, &shell_args, &working_dir, timeout)
            })
            .await
            .map_err(|e| ToolError::Internal(format!("pty task failed: {e}")))?
            .map_err(ToolError::Internal)?;

            // A timeout must surface as `Err(ToolError::Timeout)`, the contract
            // the Unix path below already honours. Returning `Ok(ToolResult::err)`
            // here made Windows callers see a run that merely "failed" instead of
            // one that ran out of time.
            let Some(code) = exit_code else {
                return Err(ToolError::Timeout(timeout_secs));
            };
            return Ok(if code == 0 {
                ToolResult::ok(output)
            } else {
                let mut msg = format!("Command exited with code {code}");
                if output.trim().is_empty() {
                    msg.push_str(" (no output captured)");
                }
                ToolResult::err(output, msg)
            });
        }

        let mut cmd = Command::new(&shell);
        cmd.args(&shell_args)
            .current_dir(&ctx.working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);

        // Hide the console window when the desktop app spawns a shell on Windows.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.as_std_mut().creation_flags(CREATE_NO_WINDOW);
        }

        // T3: Set process group so we can kill the entire process tree on timeout
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = cmd.spawn().map_err(|e| {
            ToolError::Internal(format!("Failed to spawn command: {}", e))
        })?;

        // process_group(0) ⇒ child's pid is the process-group id. Capture before wait/reap.
        let pgid: Option<u32> = child.id();

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (tx_out, mut rx_out) = tokio::sync::mpsc::unbounded_channel();
        let (tx_err, mut rx_err) = tokio::sync::mpsc::unbounded_channel();

        // T2: Keep handles to reader tasks so we can await them before draining
        let mut stdout_handle: Option<JoinHandle<()>> = None;
        let mut stderr_handle: Option<JoinHandle<()>> = None;

        // The output cap lives in the readers, not in the drain below: waiting
        // for `child.wait()` and *then* checking `MAX_OUTPUT_BYTES` means
        // `cat huge.log` has already buffered everything in the channels.
        // Each reader reserves against its own counter and stops forwarding
        // (while still draining the pipe) once its budget is gone.
        let out_bytes = Arc::new(AtomicUsize::new(0));
        let err_bytes = Arc::new(AtomicUsize::new(0));
        let truncated = Arc::new(AtomicBool::new(false));

        // Read stdout in a background task.
        //
        // Byte-oriented on purpose: `BufReader::lines()` returns `Err` on the
        // first invalid UTF-8 byte, and the old `while let Ok(Some(line))`
        // silently ended the task there — every later line was lost even when
        // the text was fine. `read_until` yields the raw bytes (terminator
        // included) for `decode_output` to make sense of.
        if let Some(stdout) = stdout {
            let tx = tx_out.clone();
            let sender = ctx.sender.clone();
            let call_id = ctx.tool_call_id.clone();
            let counter = Arc::clone(&out_bytes);
            let truncated = Arc::clone(&truncated);
            stdout_handle = Some(tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut buf: Vec<u8> = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf).await {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                    let chunk = decode_output(&buf);
                    if !reserve_output(&counter, chunk.len(), &truncated) {
                        continue; // over budget: keep draining, stop forwarding
                    }
                    let _ = sender.send(StreamEvent::ToolProgress {
                        id: call_id.clone(),
                        chunk: chunk.clone(),
                    });
                    let _ = tx.send(chunk);
                }
            }));
        }

        // Read stderr in a background task
        if let Some(stderr) = stderr {
            let tx = tx_err.clone();
            let sender = ctx.sender.clone();
            let call_id = ctx.tool_call_id.clone();
            let counter = Arc::clone(&err_bytes);
            let truncated = Arc::clone(&truncated);
            stderr_handle = Some(tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut buf: Vec<u8> = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf).await {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                    let line = decode_output(&buf);
                    let chunk = format!("[stderr] {line}");
                    if !reserve_output(&counter, chunk.len(), &truncated) {
                        continue;
                    }
                    let _ = sender.send(StreamEvent::ToolProgress {
                        id: call_id.clone(),
                        chunk: chunk.clone(),
                    });
                    let _ = tx.send(chunk);
                }
            }));
        }

        // Wait for the command with timeout
        let exit_status = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait())
            .await;

        // Always reap the process group after the shell exits (or on timeout).
        //
        // Bug we hit: `npx vite &; …; echo done` — bash prints "done" and exits, but
        // background vite still holds the inherited stdout/stderr write ends. Reader
        // tasks then never see EOF → do_bash hangs "running" forever (until tool timeout).
        // Killing the process group closes those pipes so readers finish.
        if exit_status.is_err() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        kill_process_tree(pgid);

        // Drop our channel senders (reader tasks hold the other clones until they exit).
        drop(tx_out);
        drop(tx_err);

        // Bounded wait for readers — never block the tool on orphan pipe holders.
        let reader_deadline = Duration::from_secs(2);
        let readers = async {
            if let Some(h) = stdout_handle.as_mut() {
                let _ = h.await;
            }
            if let Some(h) = stderr_handle.as_mut() {
                let _ = h.await;
            }
        };
        if tokio::time::timeout(reader_deadline, readers).await.is_err() {
            warn!(
                "do_bash: stdout/stderr readers still open after {reader_deadline:?} \
                 (orphaned background process holding pipes?); collecting partial output"
            );
            // Dropping a JoinHandle does not cancel the task: an orphan holding
            // the pipe would keep both readers — and their buffers — alive for
            // the rest of the process's life. Abort them explicitly.
            if let Some(h) = stdout_handle.as_ref() {
                h.abort();
            }
            if let Some(h) = stderr_handle.as_ref() {
                h.abort();
            }
        }

        match exit_status {
            Ok(Ok(status)) => {
                let mut output = String::new();
                let mut total_bytes: usize = 0;
                while let Ok(chunk) = rx_out.try_recv() {
                    total_bytes += chunk.len();
                    if total_bytes > MAX_OUTPUT_BYTES {
                        // Only reachable when stdout + stderr together cross the
                        // cap; each channel is already capped on its own in the
                        // reader tasks above.
                        truncated.store(true, Ordering::Relaxed);
                        break;
                    }
                    output.push_str(&chunk);
                }
                while total_bytes <= MAX_OUTPUT_BYTES {
                    match rx_err.try_recv() {
                        Ok(chunk) => {
                            total_bytes += chunk.len();
                            if total_bytes > MAX_OUTPUT_BYTES {
                                truncated.store(true, Ordering::Relaxed);
                                break;
                            }
                            output.push_str(&chunk);
                        }
                        Err(_) => break,
                    }
                }
                if truncated.load(Ordering::Relaxed) {
                    output.push_str("\n[output truncated at 10MB]\n");
                }

                let success = status.success();
                let exit_code = status.code().unwrap_or(-1);

                let result = if success {
                    ToolResult::ok(output)
                } else {
                    let mut msg = format!("Command exited with code {exit_code}");
                    if output.trim().is_empty() {
                        // Windows: some commands write via the console API
                        // (WriteConsole) instead of stdout/stderr, so pipes see
                        // nothing. Flag it instead of failing silently.
                        msg.push_str(
                            " (no stdout/stderr captured — the command may have \
                             written via the console API rather than pipes)",
                        );
                    }
                    ToolResult::err(output, msg)
                };

                Ok(result)
            }
            Ok(Err(e)) => {
                let msg = format!("Command failed: {}", e);
                Ok(ToolResult::err("", msg))
            }
            Err(_elapsed) => {
                // Process was already killed in the pre-await block above.

                // Drain any remaining output
                while let Ok(chunk) = rx_out.try_recv() {
                    // discard
                    let _ = chunk;
                }
                while let Ok(chunk) = rx_err.try_recv() {
                    let _ = chunk;
                }

                Err(ToolError::Timeout(timeout_secs))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `do_bash` resolves to a POSIX shell on this host. The command
    /// syntax in these tests is bash's; on a Windows box with no Git Bash,
    /// `shell_command` falls through to PowerShell and that syntax is invalid.
    fn resolved_shell_is_bash() -> bool {
        let (shell, _) = shell_command("probe");
        std::path::Path::new(&shell)
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("bash"))
    }

    #[test]
    fn strip_ansi_removes_csi_osc_and_crlf() {
        // ConPTY preamble (cursor/screen modes) followed by a colored line.
        let raw = "\x1b[?9001h\x1b[?25l\x1b[2J\x1b[m\x1b[38;5;9mred\x1b[0m\r\nplain\r\n";
        assert_eq!(strip_ansi(raw), "red\nplain\n");

        // OSC terminated by BEL, then by ST.
        assert_eq!(strip_ansi("\x1b]0;title\x07done"), "done");
        assert_eq!(strip_ansi("\x1b]0;title\x1b\\done"), "done");

        // Text without escapes is untouched.
        assert_eq!(strip_ansi("hello\n"), "hello\n");
    }

    #[test]
    fn strip_ansi_gives_up_on_overlong_or_unterminated_csi() {
        // An unterminated CSI (the 3 s flush deadline can cut mid-escape) must
        // not swallow the text after it — everything up to the limit is emitted
        // verbatim and scanning resumes.
        let raw = format!("\x1b[{}tail", "1".repeat(40));
        let out = strip_ansi(&raw);
        assert!(out.ends_with("tail"), "output={out:?}");
        assert!(out.starts_with("\u{1b}["), "output={out:?}");

        // A trailing lone CSI is kept rather than silently dropped.
        assert_eq!(strip_ansi("text\x1b["), "text\x1b[");
    }

    /// The bytes a real session actually stored, verbatim.
    ///
    /// Pulled from `~/.dscode/sessions.db` — 1101 tool messages from
    /// 2026-08-20 to 2026-09-11 carry a raw ConPTY preamble like this, which
    /// reached both the model's context and the tool card ("奇怪的符号"). The
    /// payload between the escapes is a shell's own output; everything around it
    /// is the pty's handshake and PowerShell's console-title write.
    #[test]
    fn strip_ansi_removes_a_real_conpty_session_preamble() {
        // `do_bash` → `Start-Sleep -Seconds 25; "waited"` through the PowerShell
        // branch: pty preamble, the command's output, PowerShell setting the
        // console title, cursor restore, then the pty's teardown.
        let raw = "\x1b[?9001h\x1b[?1004h\x1b[?25l\x1b[2J\x1b[m\x1b[Hwaited\
                   \x1b]0;C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\x07\
                   \x1b[?25h\r\n\x1b[?9001l\x1b[?1004l";
        assert_eq!(strip_ansi(raw), "waited\n");

        // The same shapes plus absolute cursor addressing (`ESC[9;1H`), which is
        // how a long PowerShell output repositions between lines. The 2026-09-02
        // session that reported this had four such moves in one result.
        let raw = "\x1b[?9001h\x1b[?25l\x1b[2J\x1b[m\x1b[Hinputs [('Q', Single)]\r\n\
                   \x1b]0;C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\x07\
                   \x1b[?25h\x1b[?25l\x1b[9;1HComputes scaled dot product\r\n\
                   \x1b[?9001l\x1b[?1004l";
        assert_eq!(
            strip_ansi(raw),
            "inputs [('Q', Single)]\nComputes scaled dot product\n"
        );
    }

    #[test]
    fn decode_output_prefers_utf8_then_gbk_then_lossy() {
        // 1. Valid UTF-8 passes through byte-for-byte.
        assert_eq!(decode_output("中文 UTF-8".as_bytes()), "中文 UTF-8");

        // 2. CP936/GBK bytes (中 = D6D0, 文 = CEC4) are not valid UTF-8, so the
        //    GBK fallback must be what recovers them — this is the user-reported
        //    garbling: with `from_utf8_lossy` these became U+FFFD U+FFFD.
        assert_eq!(decode_output(&[0xD6, 0xD0, 0xCE, 0xC4]), "中文");

        // 3. Undecodable bytes fall through to lossy rather than dropping the
        //    rest of the stream.
        let out = decode_output(&[b'A', 0xFF, b'\n', b'B']);
        assert!(out.starts_with('A') && out.ends_with('B'), "out={out:?}");
    }

    #[tokio::test]
    async fn test_bash_simple_echo() {
        let tool = DoBash::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "test".into(),
            tool_call_id: "call_echo".into(),
            sender: tx,
            safety_guard: std::sync::Arc::new(crate::safety::guard::SafetyGuard::new(&[], true)),
        permission_hub: None,
        permission_timeout_secs: 120,
        team_agent_id: None,
        file_ownership: None,
        ownership_enforced: false,
        ownership_soft_log_only: true,
        read_paths: None,
        read_before_edit: false,
        };

        let result = tool
            .execute(
                serde_json::json!({
                    "command": "echo hello",
                    "description": "Test echo"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "success=false output={:?} error={:?}",
            result.output, result.error
        );
        assert!(result.output.contains("hello"), "output={:?}", result.output);
    }

    /// Background jobs inherit stdout; without process-group cleanup, readers hang.
    #[tokio::test]
    async fn test_bash_background_job_does_not_hang_tool() {
        // `cmd &` is bash syntax. With no Git Bash on Windows, do_bash resolves
        // to PowerShell, where `&` is a parse error — the orphaned-pipe scenario
        // this test covers cannot be expressed there.
        if !resolved_shell_is_bash() {
            eprintln!(
                "skipping test_bash_background_job_does_not_hang_tool: \
                 do_bash resolved to a non-POSIX shell on this host"
            );
            return;
        }

        let tool = DoBash::with_timeout(15);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "test".into(),
            tool_call_id: "call_bg".into(),
            sender: tx,
            safety_guard: std::sync::Arc::new(crate::safety::guard::SafetyGuard::new(&[], true)),
        permission_hub: None,
        permission_timeout_secs: 120,
        team_agent_id: None,
        file_ownership: None,
        ownership_enforced: false,
        ownership_soft_log_only: true,
        read_paths: None,
        read_before_edit: false,
        };

        let started = std::time::Instant::now();
        let result = tool
            .execute(
                serde_json::json!({
                    "command": "sleep 120 & echo --- done ---",
                    "description": "Background sleep must not hang do_bash"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            started.elapsed() < Duration::from_secs(8),
            "do_bash hung on background job ({:?})",
            started.elapsed()
        );
        assert!(result.success, "output={}", result.output);
        assert!(
            result.output.contains("--- done ---"),
            "output={}",
            result.output
        );
    }

    #[tokio::test]
    async fn test_bash_failing_command() {
        let tool = DoBash::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "test".into(),
            tool_call_id: "call_fail".into(),
            sender: tx,
            safety_guard: std::sync::Arc::new(crate::safety::guard::SafetyGuard::new(&[], true)),
        permission_hub: None,
        permission_timeout_secs: 120,
        team_agent_id: None,
        file_ownership: None,
        ownership_enforced: false,
        ownership_soft_log_only: true,
        read_paths: None,
        read_before_edit: false,
        };

        let result = tool
            .execute(
                serde_json::json!({
                    "command": "exit 1",
                    "description": "Test fail"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn test_bash_empty_command() {
        let tool = DoBash::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "test".into(),
            tool_call_id: "call_empty".into(),
            sender: tx,
            safety_guard: std::sync::Arc::new(crate::safety::guard::SafetyGuard::new(&[], true)),
        permission_hub: None,
        permission_timeout_secs: 120,
        team_agent_id: None,
        file_ownership: None,
        ownership_enforced: false,
        ownership_soft_log_only: true,
        read_paths: None,
        read_before_edit: false,
        };

        let result = tool
            .execute(
                serde_json::json!({
                    "command": "   ",
                    "description": "Test empty"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let tool = DoBash::with_timeout(1);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "test".into(),
            tool_call_id: "call_timeout".into(),
            sender: tx,
            safety_guard: std::sync::Arc::new(crate::safety::guard::SafetyGuard::new(&[], true)),
        permission_hub: None,
        permission_timeout_secs: 120,
        team_agent_id: None,
        file_ownership: None,
        ownership_enforced: false,
        ownership_soft_log_only: true,
        read_paths: None,
        read_before_edit: false,
        };

        let result = tool
            .execute(
                serde_json::json!({
                    "command": "sleep 10",
                    "timeout": 1,
                    "description": "Test timeout"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::Timeout(_) => {}
            other => panic!("Expected Timeout, got {:?}", other),
        }
    }
}
