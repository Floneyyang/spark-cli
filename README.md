# Spark

A small AI assistant that works in your terminal. Ask about a project, approve a
command, inspect its real output, and follow up. Built in Rust, with three source
files and two tools. macOS and Linux only.

## Run

Install a current stable Rust toolchain from [rustup.rs](https://rustup.rs), then:

```sh
export OPENAI_API_KEY='your-api-key'
export OPENAI_MODEL='your-model-id'
cargo run
```

Choose a model available to your account that supports function calling through
the OpenAI Responses API. There is deliberately no hardcoded model default.
Variables must be exported in your shell; `.env` files are not loaded.

To use Spark from another project:

```sh
cargo install --path .
cd /path/to/your/project
spark
```

Spark uses the directory where it starts as its project directory for the session.
`spark --help` works without credentials. Startup checks configuration; the first
message makes the API connection.

## Two-minute demo

```text
you> Read Cargo.toml and explain what this project does.
  read Cargo.toml
spark> ...

you> Run the tests and explain the result.
  Directory: /your/project
  Command: cargo test
  Reason: Check the project's tests.
Run this command? [y/N] y
Exit: exit status: 0
...
spark> ...

you> Which source file should I read first, and why?
spark> ...

you> /clear
Conversation cleared.
you> /exit
```

The transcript illustrates the flow; model wording and tool choices vary.
The commands `/help`, `/clear`, and `/exit` are local. Up/down recalls input during
the session. Ctrl-C cancels an API request or running command; at the prompt it
clears input. Ctrl-D exits. Press Enter at approval to decline and stop that turn.

## How it works

1. Append your message to an in-memory conversation.
2. Send it to the model with two function definitions.
3. Display the answer, or execute a requested tool.
4. Append the tool result and ask the model to continue.

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Input, conversation, tool loop, approval |
| `src/llm.rs` | OpenAI Responses HTTP request and tool definitions |
| `src/tools.rs` | Project file reads and command execution |

The API integration follows OpenAI's [function-calling guide](https://developers.openai.com/api/docs/guides/function-calling).
All returned items, including reasoning items, are preserved in conversation
history. Responses are requested with `store: false`; history stays in memory and
is resent with each request. File reads and command results are sent to the API.

## Deliberate limits

- **Two tools:** `read_file` and `run_command`. File reads are automatic inside the
  project, including symlink resolution. Commands always require `y` or `yes`.
- **Commands are not sandboxed.** Approval permits the exact displayed command
  with your user permissions, including filesystem changes and network access.
  The API key is removed from the child environment, but other environment
  variables are inherited. Review commands and use projects you trust.
- Commands use a fresh `/bin/sh` with no interactive stdin. A `cd` affects only
  that command. Background jobs are cleaned up when the command finishes.
- 30-second command deadline, 32 KiB per output stream, 32 KiB per file read,
  120-second API deadline, and eight tool rounds followed by a summary request.
  Output beyond the limit stops the command; use focused commands or filters.
- Replies print after each API response; token streaming is deferred.
- No saved sessions, automatic context trimming, dedicated file-editing tool,
  indexing, plugins, or provider framework. Use `/clear` for a fresh context.
  An explicitly approved shell command can still edit files.

## Check

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
python3 tests/smoke.py
```

The smoke test uses a local fake Responses server and temporary projects; it
needs no API key and makes no external AI requests. It exercises the real binary,
tool-result round trips, approval/denial, follow-ups, reset, and API error recovery.
It is a protocol test, not a substitute for a live model test.

`OPENAI_BASE_URL` optionally overrides `https://api.openai.com/v1`, primarily for
the local test server. It must expose `/responses`; Spark sends the configured
API key to that endpoint.
