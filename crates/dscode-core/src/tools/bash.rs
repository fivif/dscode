//! do_bash — sandboxed shell command execution with timeout and streaming progress.

use async_trait::async_trait;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::agent::stream::{StreamEvent, ToolStatus};
use crate::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};

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

        if command.trim().is_empty() {
            return Ok(ToolResult::err("", "command must not be empty"));
        }

        let timeout_secs = args["timeout"]
            .as_u64()
            .unwrap_or(self.default_timeout_secs)
            .min(600);

        let description = args["description"]
            .as_str()
            .unwrap_or("executing command");

        // Emit ToolStart
        let _ = ctx.sender.send(StreamEvent::ToolStart {
            id: ctx.tool_call_id.clone(),
            name: self.name().into(),
            description: description.into(),
        });

        // Emit ToolProgress with a description
        let _ = ctx.sender.send(StreamEvent::ToolProgress {
            id: ctx.tool_call_id.clone(),
            chunk: format!("$ {}\n", command),
        });

        // Spawn the command
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                ToolError::Internal(format!("Failed to spawn command: {}", e))
            })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (tx_out, mut rx_out) = tokio::sync::mpsc::unbounded_channel();
        let (tx_err, mut rx_err) = tokio::sync::mpsc::unbounded_channel();

        // Read stdout in a background task
        if let Some(stdout) = stdout {
            let tx = tx_out.clone();
            let sender = ctx.sender.clone();
            let call_id = ctx.tool_call_id.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let chunk = format!("{}\n", line);
                    let _ = sender.send(StreamEvent::ToolProgress {
                        id: call_id.clone(),
                        chunk: chunk.clone(),
                    });
                    let _ = tx.send(chunk);
                }
            });
        }

        // Read stderr in a background task
        if let Some(stderr) = stderr {
            let tx = tx_err.clone();
            let sender = ctx.sender.clone();
            let call_id = ctx.tool_call_id.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let chunk = format!("[stderr] {}\n", line);
                    let _ = sender.send(StreamEvent::ToolProgress {
                        id: call_id.clone(),
                        chunk: chunk.clone(),
                    });
                    let _ = tx.send(chunk);
                }
            });
        }

        // Wait for the command with timeout
        let exit_status = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait())
            .await;

        // Drop senders so the reading tasks finish
        drop(tx_out);
        drop(tx_err);

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
                // Timeout — kill the child process
                let _ = child.start_kill();
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
