//! do_bash — sandboxed shell command execution with timeout and streaming progress.

use async_trait::async_trait;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::agent::stream::{StreamEvent, ToolStatus};
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
        "Execute a shell command in the project working directory. \
         Captures stdout and stderr. Commands are subject to a timeout. \
         Use this to run build commands, linters, tests, git operations, \
         file listing, and other shell tasks."
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

        if let Some(pattern) = is_dangerous(command) {
            return Ok(ToolResult::err(
                "",
                format!(
                    "Command blocked by safety policy: detected dangerous pattern '{}'. \
                     If you believe this is a false positive, adjust the safety config.",
                    pattern
                ),
            ));
        }

        let timeout_secs = args["timeout"]
            .as_u64()
            .unwrap_or(self.default_timeout_secs)
            .min(600);
        // T4: Minimum timeout of 5 seconds to prevent zero-timeout immediate failures
        let timeout_secs = timeout_secs.max(5);

        let description = args["description"]
            .as_str()
            .unwrap_or("executing command");

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

        // Emit ToolStart
        let _ = ctx.sender.send(StreamEvent::ToolStart {
            id: ctx.tool_call_id.clone(),
            name: self.name().into(),
            description: description.into(),
            arguments: String::new(),
        });

        // Emit ToolProgress with a description
        let _ = ctx.sender.send(StreamEvent::ToolProgress {
            id: ctx.tool_call_id.clone(),
            chunk: format!("$ {}\n", command),
        });

        // Build the command with process group support
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&ctx.working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);

        // T3: Set process group so we can kill the entire process tree on timeout
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = cmd.spawn().map_err(|e| {
            ToolError::Internal(format!("Failed to spawn command: {}", e))
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (tx_out, mut rx_out) = tokio::sync::mpsc::unbounded_channel();
        let (tx_err, mut rx_err) = tokio::sync::mpsc::unbounded_channel();

        // T2: Keep handles to reader tasks so we can await them before draining
        let mut stdout_handle: Option<JoinHandle<()>> = None;
        let mut stderr_handle: Option<JoinHandle<()>> = None;

        // Read stdout in a background task
        if let Some(stdout) = stdout {
            let tx = tx_out.clone();
            let sender = ctx.sender.clone();
            let call_id = ctx.tool_call_id.clone();
            stdout_handle = Some(tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let chunk = format!("{}\n", line);
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
            stderr_handle = Some(tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let chunk = format!("[stderr] {}\n", line);
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

        // T2: Await reader tasks so all output is consumed before we drain channels.
        // Drop the senders so channels close when tasks finish.
        drop(tx_out);
        drop(tx_err);

        // Await reader tasks to completion — they will finish when their pipes close
        if let Some(h) = stdout_handle {
            let _ = h.await;
        }
        if let Some(h) = stderr_handle {
            let _ = h.await;
        }

        match exit_status {
            Ok(Ok(status)) => {
                let mut output = String::new();
                while let Ok(chunk) = rx_out.try_recv() {
                    output.push_str(&chunk);
                }
                while let Ok(chunk) = rx_err.try_recv() {
                    output.push_str(&chunk);
                }

                let success = status.success();
                let exit_code = status.code().unwrap_or(-1);

                let result = if success {
                    ToolResult::ok(output)
                } else {
                    ToolResult::err(
                        output,
                        format!("Command exited with code {}", exit_code),
                    )
                };

                let _ = ctx.sender.send(StreamEvent::ToolEnd {
                    id: ctx.tool_call_id.clone(),
                    status: if success {
                        ToolStatus::Success
                    } else {
                        ToolStatus::Error
                    },
                    result: result.output.clone(),
                });

                Ok(result)
            }
            Ok(Err(e)) => {
                let msg = format!("Command failed: {}", e);
                let _ = ctx.sender.send(StreamEvent::ToolEnd {
                    id: ctx.tool_call_id.clone(),
                    status: ToolStatus::Error,
                    result: msg.clone(),
                });
                Ok(ToolResult::err("", msg))
            }
            Err(_elapsed) => {
                // T3: Timeout — kill the entire process group, not just the child
                #[cfg(unix)]
                {
                    let pid = child.id().unwrap_or(0) as i32;
                    if pid > 0 {
                        // SIGKILL = 9 on all unix platforms
                        const SIGKILL: i32 = 9;
                        extern "C" {
                            fn kill(pid: i32, sig: i32) -> i32;
                        }
                        // Send SIGKILL to the entire process group (negative pid = pgid)
                        unsafe { kill(-pid, SIGKILL) };
                    }
                }

                // Drain any remaining output
                while let Ok(chunk) = rx_out.try_recv() {
                    // discard
                    let _ = chunk;
                }
                while let Ok(chunk) = rx_err.try_recv() {
                    let _ = chunk;
                }

                let msg = format!("Command timed out after {}s", timeout_secs);
                let _ = ctx.sender.send(StreamEvent::ToolEnd {
                    id: ctx.tool_call_id.clone(),
                    status: ToolStatus::Error,
                    result: msg.clone(),
                });
                Err(ToolError::Timeout(timeout_secs))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_bash_simple_echo() {
        let tool = DoBash::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: "test".into(),
            tool_call_id: "call_echo".into(),
            sender: tx,
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

        assert!(result.success);
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_failing_command() {
        let tool = DoBash::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: "test".into(),
            tool_call_id: "call_fail".into(),
            sender: tx,
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
            working_dir: PathBuf::from("/tmp"),
            session_id: "test".into(),
            tool_call_id: "call_empty".into(),
            sender: tx,
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
            working_dir: PathBuf::from("/tmp"),
            session_id: "test".into(),
            tool_call_id: "call_timeout".into(),
            sender: tx,
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
