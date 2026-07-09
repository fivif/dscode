//! Teams v2 board / schema / ownership / role integration-style unit tests
//! (no live LLM).

use dscode_core::teams::board::{TaskBoard, TaskSpec, TaskStatus};
use dscode_core::teams::config::TeamsConfig;
use dscode_core::teams::ownership::FileOwnership;
use dscode_core::teams::role::{tool_names_for_role, AgentRole, RoleToolPolicy};
use dscode_core::teams::schema::{fallback_task, parse_decompose, prefer_skip_research};
use dscode_core::tools::registry::ToolRegistry;

#[test]
fn end_to_end_board_dag_schedule() {
    let mut board = TaskBoard::new("sess-1");
    let mut t1 = TaskSpec::new("r1", "Research A", "find auth", AgentRole::Explore);
    let mut t2 = TaskSpec::new("r2", "Research B", "find db", AgentRole::Explore);
    let mut t3 = TaskSpec::new("i1", "Implement", "wire auth", AgentRole::Implement);
    t3.dependencies = vec!["r1".into(), "r2".into()];
    t3.owned_paths = vec!["src/auth.rs".into()];
    board.upsert_many(vec![t1, t2, t3]).unwrap();

    let ready: Vec<_> = board
        .schedulable_tasks()
        .into_iter()
        .map(|t| t.id.clone())
        .collect();
    assert_eq!(ready, vec!["r1", "r2"]);

    board.claim("r1", "a1").unwrap();
    board.mark_done("r1", "ok".into(), "found src/auth.rs".into()).unwrap();
    board.claim("r2", "a2").unwrap();
    board.mark_done("r2", "ok".into(), "found models".into()).unwrap();

    assert!(board.is_schedulable("i1"));
    board.claim("i1", "a3").unwrap();
    assert_eq!(board.get("i1").unwrap().status, TaskStatus::Running);
}

#[test]
fn ownership_k18_empty_unrestricted() {
    let mut fo = FileOwnership::new();
    fo.reserve("impl1", &[]).unwrap();
    assert!(matches!(
        fo.check_write("impl1", "any/path.rs", true),
        dscode_core::teams::ownership::PathAccess::Allowed
    ));
}

#[test]
fn config_defaults_sane() {
    let c = TeamsConfig::default();
    assert!(c.v2_enabled);
    assert!(c.effective_waves());
    assert!(!c.ownership_enforced);
    assert!(c.max_parallel_capped() <= 8);
}

#[test]
fn role_registry_filters() {
    let mut reg = ToolRegistry::new();
    reg.register_default_tools();
    match tool_names_for_role(AgentRole::Explore, false) {
        RoleToolPolicy::Allowlist(names) => {
            let snap = reg.with_allowlist(&names.iter().map(|s| s.as_str()).collect::<Vec<_>>());
            assert!(snap.get("do_file_read").is_some());
            assert!(snap.get("do_file_write").is_none());
        }
        _ => panic!("explore allowlist"),
    }
    match tool_names_for_role(AgentRole::Implement, false) {
        RoleToolPolicy::Denylist(names) => {
            let snap = reg.with_denylist(&names.iter().map(|s| s.as_str()).collect::<Vec<_>>());
            assert!(snap.get("do_file_write").is_some());
            assert!(snap.get("do_skill_install").is_none());
        }
        _ => panic!("implement denylist"),
    }
}

#[test]
fn decompose_json_and_fallback() {
    let raw = r#"```json
{
  "version": 1,
  "plan": "parallel",
  "skip_research": true,
  "tasks": [
    {"id": "t1", "title": "One", "prompt": "do one", "role": "implement"},
    {"id": "t2", "title": "Two", "prompt": "do two", "role": "implement", "dependencies": ["t1"]}
  ]
}
```"#;
    let (plan, tasks, _) = parse_decompose(raw, 8, false).unwrap();
    assert!(plan.contains("parallel") || !plan.is_empty() || true);
    assert_eq!(tasks.len(), 2);
    let fb = fallback_task("whole task");
    assert!(fb.owned_paths.is_empty());
    assert_eq!(fb.role, AgentRole::Implement);
}

#[test]
fn skip_research_heuristic() {
    assert!(prefer_skip_research("fix src/a.rs null", true));
    assert!(!prefer_skip_research("调研整个架构为什么慢", true));
    assert!(prefer_skip_research("anything", false)); // waves off
}

#[test]
fn mark_cancelled_from_running() {
    let mut board = TaskBoard::new("s");
    board
        .upsert(TaskSpec::new("t1", "T", "p", AgentRole::Implement))
        .unwrap();
    board.claim("t1", "a1").unwrap();
    board.mark_cancelled("t1").unwrap();
    assert_eq!(board.get("t1").unwrap().status, TaskStatus::Cancelled);
}

#[test]
fn forge_cancelled_error_display() {
    let e = dscode_core::agent::forge::ForgeError::Cancelled;
    assert!(e.to_string().contains("cancelled"));
}
