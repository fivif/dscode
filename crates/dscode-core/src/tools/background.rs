//! do_background — spawn shell commands that run in the background without
//! blocking the agent loop. The agent can check status later with `do_task_status`.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};

/// A background task tracked by the task manager.
#[derive(Debug, Clone)]
pub struct BackgroundTask {
    pub id: String,
    pub description: String,
    pub command: String,
    pub status: TaskStatus,
    pub output: String,
    pub started_at: Instant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Running,
    Success,
    Failed(String),
}

/// Global background task manager.
pub struct TaskManager {
    tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self { tasks: Arc::new(Mutex::new(HashMap::new())) }
    }

    pub fn handle(&self) -> Arc<Mutex<HashMap<String, BackgroundTask>>> {
        self.tasks.clone()
    }

    /// Spawn a background command, return immediately with a task ID.
    pub async fn spawn(&self, id: String, description: String, command: String, working_dir: std::path::PathBuf) {
        let tasks = self.tasks.clone();
        let tid = id.clone();
        let desc = description.clone();
        let cmd = command.clone();

        // Register task as running
        {
            let mut guard = tasks.lock().await;
            guard.insert(id.clone(), BackgroundTask {
                id: id.clone(),
                description: desc.clone(),
                command: cmd.clone(),
                status: TaskStatus::Running,
                output: String::new(),
                started_at: Instant::now(),
            });
        }

        // Spawn the actual work
        tokio::spawn(async move {
            let result = Command::new("bash")
                .arg("-c")
                .arg(&cmd)
                .current_dir(&working_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output()
                .await;

            let mut guard = tasks.lock().await;
            if let Some(task) = guard.get_mut(&tid) {
                match result {
                    Ok(output) => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        task.output = format!("{}{}", stdout, stderr);
                        if output.status.success() {
                            task.status = TaskStatus::Success;
                        } else {
                            task.status = TaskStatus::Failed(format!(
                                "exit code {}",
                                output.status.code().unwrap_or(-1)
                            ));
                        }
                    }
                    Err(e) => {
                        task.status = TaskStatus::Failed(e.to_string());
                        task.output = e.to_string();
                    }
                }
            }
        });
    }
}

/// The `do_background` tool: spawns a command that runs without blocking.
pub struct DoBackground {
    task_manager: Arc<Mutex<HashMap<String, BackgroundTask>>>,
}

impl DoBackground {
    pub fn new(task_manager: Arc<Mutex<HashMap<String, BackgroundTask>>>) -> Self {
        Self { task_manager }
    }
}

#[async_trait]
impl Tool for DoBackground {
    fn name(&self) -> &str { "do_background" }

    fn description(&self) -> &str {
        "Run a shell command in the background. Returns immediately with a task ID. \
         Use do_task_status to check the result later. \
         Good for: npm install, pip install, cargo build, long operations. \
         NOT for: quick commands (use do_bash), commands whose output you need immediately."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to run in background." },
                "description": { "type": "string", "description": "Short description of what this task does." }
            },
            "required": ["command", "description"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let command = args["command"].as_str().ok_or(ToolError::MissingParameter("command".into()))?;
        let description = args["description"].as_str().unwrap_or("background task");
        let task_id = format!("bg_{}", uuid::Uuid::new_v4().to_string().chars().take(8).collect::<String>());

        let mgr = TaskManager { tasks: self.task_manager.clone() };
        mgr.spawn(task_id.clone(), description.into(), command.into(), ctx.working_dir.clone()).await;

        Ok(ToolResult::ok(format!(
            "Background task started: {}\nTask ID: {}\nCommand: {}\nUse do_task_status with task_id=\"{}\" to check status.",
            description, task_id, command, task_id
        )))
    }
}

/// Check status of background tasks.
pub struct DoTaskStatus {
    task_manager: Arc<Mutex<HashMap<String, BackgroundTask>>>,
}

impl DoTaskStatus {
    pub fn new(task_manager: Arc<Mutex<HashMap<String, BackgroundTask>>>) -> Self {
        Self { task_manager }
    }
}

#[async_trait]
impl Tool for DoTaskStatus {
    fn name(&self) -> &str { "do_task_status" }

    fn description(&self) -> &str {
        "Check the status of background tasks. Use task_id from do_background to check a specific task, \
         or omit to list all tasks."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Optional: specific task ID to check." }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let guard = self.task_manager.lock().await;

        if let Some(task_id) = args["task_id"].as_str() {
            match guard.get(task_id) {
                Some(task) => {
                    let elapsed = task.started_at.elapsed().as_secs();
                    let status_str = match &task.status {
                        TaskStatus::Running => format!("Running ({}s)", elapsed),
                        TaskStatus::Success => format!("Success (completed in {}s)", elapsed),
                        TaskStatus::Failed(e) => format!("Failed after {}s: {}", elapsed, e),
                    };
                    Ok(ToolResult::ok(format!(
                        "Task: {}\nStatus: {}\nCommand: {}\nOutput:\n{}",
                        task.description, status_str, task.command,
                        if task.output.is_empty() { "(no output yet)" } else { &task.output }
                    )))
                }
                None => Ok(ToolResult::err("", format!("Task '{}' not found", task_id))),
            }
        } else {
            let mut lines = vec![];
            for (_, task) in guard.iter() {
                let elapsed = task.started_at.elapsed().as_secs();
                let status = match &task.status {
                    TaskStatus::Running => format!("Running ({}s)", elapsed),
                    TaskStatus::Success => format!("Done ({}s)", elapsed),
                    TaskStatus::Failed(_) => format!("Failed"),
                };
                lines.push(format!("{} | {} | {} | {}", task.id, status, task.description, task.command.chars().take(80).collect::<String>()));
            }
            if lines.is_empty() {
                Ok(ToolResult::ok("No background tasks."))
            } else {
                Ok(ToolResult::ok(format!("ID | Status | Description | Command\n{}", lines.join("\n"))))
            }
        }
    }
}
