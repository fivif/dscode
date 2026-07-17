# Contributing to DS Code

Thank you for your interest in contributing to DS Code! This document outlines the process for contributing to the project.

---

## Table of Contents

1. [Code of Conduct](#code-of-conduct)
2. [Getting Started](#getting-started)
3. [Development Workflow](#development-workflow)
4. [Project Structure](#project-structure)
5. [Code Style](#code-style)
6. [Testing](#testing)
7. [Pull Request Process](#pull-request-process)
8. [Reporting Bugs](#reporting-bugs)
9. [Feature Requests](#feature-requests)

---

## Code of Conduct

Be respectful, collaborative, and constructive. We follow the [Contributor Covenant](https://www.contributor-covenant.org/).

---

## Getting Started

### Prerequisites

- **Rust** 1.85+ (install via [rustup](https://rustup.rs))
- **Node.js** 18+ (for desktop UI and llm_wiki)
- **npm** 9+ or **pnpm**

### Setup

```bash
# Clone the repository
git clone https://github.com/zay/dscode.git
cd dscode

# Build all Rust crates
cargo build --workspace

# Run the TUI
cargo run -p dscode-tui

# Run the CLI
cargo run -p dscode-cli -- "Hello, DS Code!"

# For desktop development:
cd crates/dscode-desktop/ui && npm install
cd ../../.. && cargo tauri dev
```

### Configuration

Create `~/.dscode/config.toml`:

```toml
default_model = "deepseek/deepseek-v4-pro"

[providers.deepseek]
api_key = "your-api-key"
base_url = "https://api.deepseek.com/v1"
enabled = true
```

See [config/default.toml](config/default.toml) for a full template.

---

## Development Workflow

### Branches

- `main` — stable, releasable code
- `feature/<name>` — new features
- `fix/<name>` — bug fixes
- `docs/<name>` — documentation changes

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(agent): add streaming token buffer
fix(tools): resolve path traversal edge case
docs(readme): update installation instructions
test(wiki): add FTS5 search integration tests
refactor(magi): extract promote scoring to separate module
```

### Before Submitting

```bash
# Format
cargo fmt --all

# Lint
cargo clippy --workspace -- -D warnings

# Test
cargo test --workspace

# Check all targets compile
cargo check --workspace --all-targets
```

---

## Project Structure

```
DS_code/
├── crates/
│   ├── dscode-core/          # Core engine library (agent, providers, tools, wiki, memory, ...)
│   ├── dscode-cli/           # CLI binary (single-message invocation)
│   ├── dscode-tui/           # Terminal UI binary (ratatui + crossterm)
│   └── dscode-desktop/       # Desktop GUI (Tauri 2.x + React 18)
│       ├── src/              #   Rust backend (commands, state, events)
│       └── ui/               #   React frontend (TypeScript + Tailwind)
├── llm_wiki/                 # Auxiliary: knowledge wiki desktop app
├── config/                   # Configuration templates
├── Cargo.toml                # Workspace root
└── Makefile                  # Build shortcuts
```

### Key Architecture Principles

1. **Headless Core**: `dscode-core` is a pure library with zero UI dependencies. All three frontends (CLI, TUI, Desktop) consume it.

2. **Trait Abstractions**:
   - `LlmProvider` — swap LLM backends (OpenAI, Anthropic, DeepSeek, Ollama)
   - `Tool` — register custom tools with sandboxed execution

3. **Event-Driven**: `StreamEvent` enum carries all agent→UI communication as a typed protocol.

4. **Safety-First**: Every tool call goes through `SafetyGuard` for command and path validation.

---

## Code Style

### Rust

- Follow [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- Use `thiserror` for library errors, `anyhow` for application errors
- Prefer `&str` over `&String` in function parameters
- Document public APIs with `///` doc comments
- Module-level docs with `//!`
- Keep functions small: target ≤30 lines, refactor if >60

### TypeScript / React

- Use functional components with hooks
- Zustand stores for global state
- Tailwind CSS utility classes (avoid custom CSS when possible)
- Type all props and store interfaces explicitly

---

## Testing

### Rust Tests

```bash
# Run all tests
cargo test --workspace

# Run specific crate
cargo test -p dscode-core

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test -p dscode-core -- tools::bash::test_dangerous_command_blocked
```

### Test Coverage Expectations

| Module | Target |
|--------|--------|
| `tools/` | ≥80% |
| `providers/` | ≥60% |
| `wiki/` | ≥60% |
| `safety/` | ≥80% |
| `agent/forge` | ≥70% (integration tests) |
| `magi/` | ≥60% (integration tests) |

### Writing Integration Tests

For modules that interact with LLMs (forge, magi, teams, plan), use a **mock `LlmProvider`**:

```rust
struct MockProvider {
    responses: Vec<ChatResponse>,
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn chat(&self, _messages: Vec<Message>, _tools: Vec<ToolDef>) -> Result<ChatResponse, ProviderError> {
        Ok(self.responses[0].clone())
    }
    // ...
}
```

---

## Pull Request Process

1. **Fork** the repository and create a feature branch
2. **Implement** your changes with tests
3. **Run** `cargo fmt --all && cargo clippy --workspace -- -D warnings && cargo test --workspace`
4. **Update** documentation if you changed public APIs
5. **Submit** a PR with a clear description:

   ```
   ## Summary
   Brief description of the change

   ## Motivation
   Why this change is needed

   ## Changes
   - Bullet list of specific changes

   ## Testing
   How you tested the changes
   ```

6. **Address** review feedback

---

## Reporting Bugs

Open an issue with:

- **Description**: what happened vs. what you expected
- **Reproduction steps**: minimal sequence to trigger the bug
- **Environment**: OS, Rust version (`rustc --version`), DS Code version
- **Logs**: any error output or trace logs (`RUST_LOG=debug`)

### Security Bugs

**Do not open a public issue for security vulnerabilities.**

See [SECURITY.md](SECURITY.md) for responsible disclosure.

---

## Feature Requests

Open an issue tagged `enhancement` with:

- **Problem**: what pain point are you experiencing?
- **Proposed solution**: how would you like DS Code to address it?
- **Alternatives considered**: other approaches you've thought about
- **Impact**: how would this improve your workflow?

---

## Questions?

Open a [discussion](https://github.com/zay/dscode/discussions) or ask in the issue tracker.

---

> Thanks for contributing! 🚀
