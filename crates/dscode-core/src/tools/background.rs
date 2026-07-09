//! do_background / do_task_status / do_task_kill — long-running shell tasks.
//!
//! Use **do_background** for servers and installs that must not block the agent
//! (`vite`, `npm run dev`, `cargo watch`, …). Returns a task id immediately.
//! Use **do_task_status** to inspect output and **do_task_kill** to stop a task.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, Mutex};

use crate::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};

const MAX_LOG_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone)]
pub struct BackgroundTask {
    pub id: String,
    pub description: String,
    pub command: String,
    pub status: TaskStatus,
    pub output: String,
    pub started_at: Instant,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Running,
    Success,
    Failed(String),
    Killed,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskNotification {
    pub task_id: String,
    pub status: TaskNotificationStatus,
    pub output: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum TaskNotificationStatus {
    Started,
    Progress,
    Completed,
    #[serde(rename = "Failed")]
    Failed(String),
    Killed,
}

/// Live OS process so we can kill it later.
pub struct LiveChild {
    pub child: Child,
    #[cfg(unix)]
    pub pgid: u32,
}

pub struct TaskManager {
    tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>,
    live: Arc<Mutex<HashMap<String, LiveChild>>>,
    notify_tx: broadcast::Sender<TaskNotification>,
}

impl TaskManager {
    pub fn new() -> Self {
        let (notify_tx, _) = broadcast::channel(64);
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            live: Arc::new(Mutex::new(HashMap::new())),
            notify_tx,
        }
    }

    pub fn handle(&self) -> Arc<Mutex<HashMap<String, BackgroundTask>>> {
        self.tasks.clone()
    }

    pub fn live_handle(&self) -> Arc<Mutex<HashMap<String, LiveChild>>> {
        self.live.clone()
    }

    pub fn notify_tx(&self) -> broadcast::Sender<TaskNotification> {
        self.notify_tx.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskNotification> {
        self.notify_tx.subscribe()
    }

    pub async fn spawn(
        &self,
        id: String,
        description: String,
        command: String,
        working_dir: std::path::PathBuf,
    ) {
        let tasks = self.tasks.clone();
        let live = self.live.clone();
        let tid = id.clone();
        let notify = self.notify_tx.clone();

        {
            let mut guard = tasks.lock().await;
            guard.insert(
                id.clone(),
                BackgroundTask {
                    id: id.clone(),
                    description: description.clone(),
                    command: command.clone(),
                    status: TaskStatus::Running,
                    output: String::new(),
                    started_at: Instant::now(),
                    pid: None,
                },
            );
        }

        let _ = notify.send(TaskNotification {
            task_id: tid.clone(),
            status: TaskNotificationStatus::Started,
            output: String::new(),
        });

        tokio::spawn(async move {
            let mut builder = Command::new("bash");
            builder
                .arg("-c")
                .arg(&command)
                .current_dir(&working_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);

            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                builder.process_group(0);
            }

            let mut child = match builder.spawn() {
                Ok(c) => c,
                Err(e) => {
                    fail_task(&tasks, &notify, &tid, e.to_string()).await;
                    return;
                }
            };

            let pid = child.id();
            #[cfg(unix)]
            let pgid = pid.unwrap_or(0);

            {
                let mut guard = tasks.lock().await;
                if let Some(t) = guard.get_mut(&tid) {
                    t.pid = pid;
                }
            }

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            {
                let mut g = live.lock().await;
                g.insert(
                    tid.clone(),
                    LiveChild {
                        child,
                        #[cfg(unix)]
                        pgid,
                    },
                );
            }

            let t1 = tasks.clone();
            let n1 = notify.clone();
            let id1 = tid.clone();
            let out_h = tokio::spawn(async move {
                pipe_to_log(stdout, false, &t1, &n1, &id1).await;
            });
            let t2 = tasks.clone();
            let n2 = notify.clone();
            let id2 = tid.clone();
            let err_h = tokio::spawn(async move {
                pipe_to_log(stderr, true, &t2, &n2, &id2).await;
            });

            let wait_res = {
                let mut g = live.lock().await;
                if let Some(lc) = g.get_mut(&tid) {
                    lc.child.wait().await
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "task already removed",
                    ))
                }
            };

            let _ = out_h.await;
            let _ = err_h.await;
            live.lock().await.remove(&tid);

            let mut guard = tasks.lock().await;
            let Some(task) = guard.get_mut(&tid) else {
                return;
            };
            if task.status == TaskStatus::Killed {
                let _ = notify.send(TaskNotification {
                    task_id: tid.clone(),
                    status: TaskNotificationStatus::Killed,
                    output: task.output.clone(),
                });
                return;
            }
            match wait_res {
                Ok(st) if st.success() => {
                    task.status = TaskStatus::Success;
                    let _ = notify.send(TaskNotification {
                        task_id: tid.clone(),
                        status: TaskNotificationStatus::Completed,
                        output: task.output.clone(),
                    });
                }
                Ok(st) => {
                    let err = format!("exit code {}", st.code().unwrap_or(-1));
                    task.status = TaskStatus::Failed(err.clone());
                    let _ = notify.send(TaskNotification {
                        task_id: tid.clone(),
                        status: TaskNotificationStatus::Failed(err),
                        output: task.output.clone(),
                    });
                }
                Err(e) => {
                    task.status = TaskStatus::Failed(e.to_string());
                    let _ = notify.send(TaskNotification {
                        task_id: tid.clone(),
                        status: TaskNotificationStatus::Failed(e.to_string()),
                        output: task.output.clone(),
                    });
                }
            }
        });
    }

    pub async fn kill(&self, task_id: &str) -> Result<String, String> {
        let pid = {
            let mut tasks = self.tasks.lock().await;
            let task = tasks
                .get_mut(task_id)
                .ok_or_else(|| format!("Task '{task_id}' not found"))?;
            if task.status != TaskStatus::Running {
                return Ok(format!(
                    "Task {task_id} is not running (already {:?})",
                    task.status
                ));
            }
            task.status = TaskStatus::Killed;
            task.pid
        };

        {
            let mut live = self.live.lock().await;
            if let Some(mut lc) = live.remove(task_id) {
                let _ = lc.child.start_kill();
            }
        }

        #[cfg(unix)]
        if let Some(pid) = pid.filter(|&p| p > 1) {
            kill_pg(pid);
        }

        let _ = self.notify_tx.send(TaskNotification {
            task_id: task_id.to_string(),
            status: TaskNotificationStatus::Killed,
            output: String::new(),
        });

        Ok(format!("Killed background task {task_id}"))
    }
}

async fn fail_task(
    tasks: &Arc<Mutex<HashMap<String, BackgroundTask>>>,
    notify: &broadcast::Sender<TaskNotification>,
    tid: &str,
    err: String,
) {
    let mut guard = tasks.lock().await;
    if let Some(task) = guard.get_mut(tid) {
        task.status = TaskStatus::Failed(err.clone());
        task.output = err.clone();
    }
    let _ = notify.send(TaskNotification {
        task_id: tid.to_string(),
        status: TaskNotificationStatus::Failed(err.clone()),
        output: err,
    });
}

async fn pipe_to_log(
    pipe: Option<impl tokio::io::AsyncRead + Unpin>,
    is_stderr: bool,
    tasks: &Arc<Mutex<HashMap<String, BackgroundTask>>>,
    notify: &broadcast::Sender<TaskNotification>,
    tid: &str,
) {
    let Some(pipe) = pipe else { return };
    let mut reader = BufReader::new(pipe).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        let chunk = if is_stderr {
            format!("[stderr] {line}\n")
        } else {
            format!("{line}\n")
        };
        {
            let mut guard = tasks.lock().await;
            if let Some(task) = guard.get_mut(tid) {
                task.output.push_str(&chunk);
                if task.output.len() > MAX_LOG_BYTES {
                    let drain = task.output.len() - MAX_LOG_BYTES;
                    task.output.drain(..drain);
                    task.output.insert_str(0, "…[log truncated]…\n");
                }
            }
        }
        let _ = notify.send(TaskNotification {
            task_id: tid.to_string(),
            status: TaskNotificationStatus::Progress,
            output: chunk,
        });
    }
}

#[cfg(unix)]
fn kill_pg(pgid: u32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;
    let p = pgid as i32;
    let _ = unsafe { kill(-p, SIGTERM) };
    std::thread::sleep(std::time::Duration::from_millis(100));
    let _ = unsafe { kill(-p, SIGKILL) };
}

// ── Tools ──────────────────────────────────────────────────────────────────

pub struct DoBackground {
    tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>,
    live: Arc<Mutex<HashMap<String, LiveChild>>>,
    notify_tx: broadcast::Sender<TaskNotification>,
}

impl DoBackground {
    pub fn new(
        tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>,
        live: Arc<Mutex<HashMap<String, LiveChild>>>,
        notify_tx: broadcast::Sender<TaskNotification>,
    ) -> Self {
        Self {
            tasks,
            live,
            notify_tx,
        }
    }
}

#[async_trait]
impl Tool for DoBackground {
    fn name(&self) -> &str {
        "do_background"
    }

    fn description(&self) -> &str {
        "Start a long-running shell command in the background; returns immediately with task_id. \
         ALWAYS use this (NOT do_bash) for dev servers/watchers: vite, next dev, npm run dev, \
         cargo watch, python -m http.server, docker compose up, etc. \
         do_task_status = logs; do_task_kill = stop. \
         For short commands that must finish before you continue, use do_bash."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run in background." },
                "description": { "type": "string", "description": "Short label, e.g. 'vite dev server'." }
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
        let description = args["description"].as_str().unwrap_or("background task");
        let task_id = format!(
            "bg_{}",
            uuid::Uuid::new_v4()
                .to_string()
                .chars()
                .take(8)
                .collect::<String>()
        );

        let mgr = TaskManager {
            tasks: self.tasks.clone(),
            live: self.live.clone(),
            notify_tx: self.notify_tx.clone(),
        };
        mgr.spawn(
            task_id.clone(),
            description.into(),
            command.into(),
            ctx.working_dir.clone(),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let preview = {
            let g = self.tasks.lock().await;
            g.get(&task_id)
                .map(|t| {
                    if t.output.is_empty() {
                        "(starting…)".to_string()
                    } else {
                        t.output.chars().take(800).collect()
                    }
                })
                .unwrap_or_else(|| "(starting…)".into())
        };

        Ok(ToolResult::ok(format!(
            "Background task started: {description}\n\
             Task ID: {task_id}\n\
             Command: {command}\n\
             Working dir: {}\n\n\
             Early log:\n{preview}\n\n\
             → do_task_status(task_id=\"{task_id}\") for more logs\n\
             → do_task_kill(task_id=\"{task_id}\") to stop",
            ctx.working_dir.display()
        )))
    }
}

pub struct DoTaskStatus {
    tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>,
}

impl DoTaskStatus {
    pub fn new(tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for DoTaskStatus {
    fn name(&self) -> &str {
        "do_task_status"
    }

    fn description(&self) -> &str {
        "Check status and recent logs of do_background tasks. \
         Pass task_id for one task, or omit to list all."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Optional task id." }
            },
            "required": []
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let guard = self.tasks.lock().await;

        if let Some(task_id) = args["task_id"].as_str() {
            match guard.get(task_id) {
                Some(task) => {
                    let elapsed = task.started_at.elapsed().as_secs();
                    let status_str = match &task.status {
                        TaskStatus::Running => format!("Running ({elapsed}s)"),
                        TaskStatus::Success => format!("Success ({elapsed}s)"),
                        TaskStatus::Failed(e) => format!("Failed after {elapsed}s: {e}"),
                        TaskStatus::Killed => format!("Killed after {elapsed}s"),
                    };
                    let pid = task.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
                    Ok(ToolResult::ok(format!(
                        "Task: {}\nID: {}\nPID: {}\nStatus: {}\nCommand: {}\nOutput:\n{}",
                        task.description,
                        task.id,
                        pid,
                        status_str,
                        task.command,
                        if task.output.is_empty() {
                            "(no output yet)"
                        } else {
                            &task.output
                        }
                    )))
                }
                None => Ok(ToolResult::err("", format!("Task '{task_id}' not found"))),
            }
        } else {
            let mut lines = vec!["ID | Status | PID | Description | Command".to_string()];
            for (_, task) in guard.iter() {
                let elapsed = task.started_at.elapsed().as_secs();
                let status = match &task.status {
                    TaskStatus::Running => format!("Running ({elapsed}s)"),
                    TaskStatus::Success => format!("Done ({elapsed}s)"),
                    TaskStatus::Failed(_) => "Failed".into(),
                    TaskStatus::Killed => "Killed".into(),
                };
                let pid = task.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
                lines.push(format!(
                    "{} | {} | {} | {} | {}",
                    task.id,
                    status,
                    pid,
                    task.description,
                    task.command.chars().take(80).collect::<String>()
                ));
            }
            if lines.len() == 1 {
                Ok(ToolResult::ok("No background tasks."))
            } else {
                Ok(ToolResult::ok(lines.join("\n")))
            }
        }
    }
}

pub struct DoTaskKill {
    tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>,
    live: Arc<Mutex<HashMap<String, LiveChild>>>,
    notify_tx: broadcast::Sender<TaskNotification>,
}

impl DoTaskKill {
    pub fn new(
        tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>,
        live: Arc<Mutex<HashMap<String, LiveChild>>>,
        notify_tx: broadcast::Sender<TaskNotification>,
    ) -> Self {
        Self {
            tasks,
            live,
            notify_tx,
        }
    }
}

#[async_trait]
impl Tool for DoTaskKill {
    fn name(&self) -> &str {
        "do_task_kill"
    }

    fn description(&self) -> &str {
        "Stop a background task (SIGTERM/SIGKILL process group). \
         Use to shut down vite/dev servers started with do_background."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Task id from do_background." }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let task_id = args["task_id"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("task_id".into()))?;
        let mgr = TaskManager {
            tasks: self.tasks.clone(),
            live: self.live.clone(),
            notify_tx: self.notify_tx.clone(),
        };
        match mgr.kill(task_id).await {
            Ok(msg) => Ok(ToolResult::ok(msg)),
            Err(e) => Ok(ToolResult::err("", e)),
        }
    }
}
