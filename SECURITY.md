# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

**Do not open a public issue.** Instead, report vulnerabilities privately:

- **Email**: security@dscode.dev (or open a private security advisory on GitHub)

You will receive a response within **48 hours**. We aim to patch confirmed vulnerabilities within **7 days**.

### What to Include

- Description of the vulnerability
- Steps to reproduce
- Affected components/versions
- Potential impact
- Suggested fix (if available)

---

## Security Model

DS Code executes AI-generated tool calls on your system. Our security model has multiple layers:

### 1. Command Execution (`do_bash`)

- **Static Blacklist**: blocks `rm -rf /`, `mkfs.*`, `dd if=`, fork bombs (`:(){ :|:& };:`), `chmod -R 777 /`, `sudo rm`, `sudo mv`
- **Regex Guard**: `SafetyGuard` applies user-configurable regex patterns to block additional dangerous commands
- **Process Isolation**: commands run in isolated process groups, killed via `kill(-pid, SIGKILL)` on drop
- **Timeout**: default 120s, configurable, hard cap at 600s
- **Output Limit**: 10MB max combined stdout+stderr

### 2. File System (`do_file_read/write/edit`)

- **Path Traversal Prevention**: all paths are canonicalized and checked to be within the working directory
- **Symlink Resolution**: symbolic links are resolved before boundary checks
- **Atomic Edits**: `do_file_edit` requires exact unique match, preventing unintended modifications
- **Non-Existent Path Safety**: `resolve_safe_path` handles paths to files that don't exist yet

### 3. API Keys

- Stored in `~/.dscode/config.toml`
- File permissions should be `600` (owner read/write only)
- **Known limitation**: keys are stored in plaintext. System keyring integration is planned (see [#P2-14](CODE_QUALITY_REPORT.md)).

### 4. MCP (Model Context Protocol)

- MCP servers run as child processes over stdio
- 30-second initialization timeout
- Tool discovery is explicit (`tools/list` handshake)
- Only tools returned by the server are callable

### 5. Context Window

- Compression pipeline prevents token overflow attacks
- L1–L4 escalation: truncation → summarization → wiki ingestion → hard cutoff
- Tool-call ↔ tool-result chain alignment is preserved during compression

---

## Security Best Practices for Users

1. **File Permissions**: `chmod 600 ~/.dscode/config.toml`
2. **API Keys**: use read-only API keys with minimal scope when possible
3. **Working Directory**: run DS Code from the specific project directory, not from `/` or `~`
4. **Command Whitelist**: for production environments, configure `safety.allowed_commands` in config
5. **Review Tool Calls**: the TUI and Desktop UI display tool calls before execution — review them
6. **Keep Updated**: always run the latest version for security patches

---

## Audit

The codebase has been manually audited for:

- Command injection vulnerabilities
- Path traversal attacks
- Unsafe Rust usage (1 instance, justified: Unix `kill(-pid, SIGKILL)`)
- Dependency license compliance (0 GPL/AGPL risks)

### Unsafe Code

| Location | Lines | Purpose | Risk |
|----------|-------|---------|------|
| `tools/bash.rs` | 1 | `kill(-pid, SIGKILL)` for process group cleanup | Low — well-isolated, no memory safety impact |

---

## Responsible Disclosure Hall of Fame

(Empty — be the first!)

---

> Last updated: 2025-07-09
