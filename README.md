# Spark

A small **shell written in Rust, with AI assistance**. Type ordinary commands to
run programs directly. Type `?` or `? your question` to enter AI chat, then keep
talking without repeating `?`. Use `/shell` to return to commands. macOS and Linux only.

```text
spark-shell> ls
Cargo.toml  src  README.md
spark-shell> cd src
spark-shell> pwd
/your/project/src
spark-shell> ? explain main.rs
  read main.rs
spark> ...
spark> which function should I read first?
spark> ...
spark> /shell
spark-shell> pwd
/your/project/src
```

## Run

Install a current stable Rust toolchain from [rustup.rs](https://rustup.rs), then:

```sh
cargo run
```

The shell works without credentials. To enable AI, export a key in your existing
terminal before starting Spark:

```sh
export OPENAI_API_KEY='your-api-key'
cargo run
```

Spark defaults to [`gpt-5.4-mini`](https://developers.openai.com/api/docs/models/gpt-5.4-mini).
Your account needs access to the model. Optionally export
`OPENAI_MODEL='your-model-id'` to choose another Responses-compatible model.
`.env` files are not loaded automatically; source your local `.env` from your
existing shell before launching Spark if you keep the key there.

To use Spark from another directory:

```sh
cargo install --path .
cd /path/to/your/project
spark
```

## Two-minute demo

```text
spark-shell> pwd
/your/project

spark-shell> cargo test
...real test output, directly in your terminal...

spark-shell> ? Read Cargo.toml and explain this project.
  read Cargo.toml
spark> ...

spark> Run the tests and explain the result.
  Directory: /your/project
  Command: cargo test
  Reason: Check the project's tests.
Run this command? [y/N] y
Exit: exit status: 0
...
spark> ...

spark> Which source file should I read first?
spark> ...

spark> /shell
spark-shell> exit
```

AI wording and tool choices vary. **Only AI-requested commands need approval.**
In shell mode, ordinary commands execute immediately without contacting the model.
In AI mode, all ordinary text is a chat message; switch back with `/shell` before
typing a command you want to execute directly. The prompts are `spark-shell>` for shell mode and `spark>` for AI mode. Use `pwd` to see the current directory.
AI mode persists after replies, errors, declined commands, and Ctrl-C.

## What the shell supports

- Launching programs from `PATH` or an explicit executable path, with arguments.
- Single/double quotes, empty quoted arguments, and backslash escapes.
- Builtins: `cd [directory]`, `pwd`, and `exit`. `cd` persists across commands;
  `cd` alone goes home, and `cd ~/directory` works.
- Foreground programs inherit terminal input and output, so commands such as
  `cat` can read input. Ctrl-C interrupts the foreground command and returns to
  the prompt. There is no job control or background-job management.
- Up/down recalls input for the current session. Ctrl-D exits at the prompt.
- `help` in shell mode, or `/help` in either mode, shows usage.
- `/shell` returns to commands and keeps the AI conversation for later.
  Enter `?` again to resume it. `/clear` resets the conversation without changing mode.
- `/exit` and `/quit` quit Spark from either mode. Plain `exit` quits in shell mode.

This is a deliberately small command language. **Pipes, redirection, chaining,
globbing, variable expansion, command substitution, scripts, `source`, and
`export` are not implemented yet.** Unsupported syntax is rejected instead of
being passed to another shell. Use separate commands and explicit paths.

## How it works

```text
Ordinary command -> Rust argument parser -> builtin or direct process launch
AI chat message -> model -> tool request -> approval for commands -> direct launch
                         <- actual tool result <-----------------------------+
```

Spark launches executables with `Command::new(program).args(arguments)`.
It does **not** automatically wrap commands in `/bin/sh -c`. The user can still
explicitly launch another shell as a program, just as with any other executable.

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Prompt, command/AI routing, conversation, approval |
| `src/shell.rs` | Argument parsing, builtins, foreground process execution |
| `src/llm.rs` | OpenAI Responses HTTP request and tool definitions |
| `src/tools.rs` | AI file reads and bounded command capture |

The API integration follows OpenAI's [function-calling guide](https://developers.openai.com/api/docs/guides/function-calling).
AI history contains chat messages and tool results, including reasoning
items. Ordinary command output stays in the terminal and is not included in AI
history. Responses use `store: false`; history is held in memory and resent.
Changing directory clears AI history so the next request starts in the new
directory. Automatic file reads are limited to that directory and descendants,
including after symlink resolution.

## AI execution limits

- The AI has two tools: `read_file` and `run_command`. File reads are automatic;
  commands require `y` or `yes`. Declining stops the turn.
- AI commands use the same Rust argument parser. They run without interactive
  stdin, with a 30-second deadline and 32 KiB per output stream. They cannot
  invoke Spark's `cd` or `exit` builtins; use those at the prompt.
- AI commands run in their own process group, which is cleaned up after
  completion or cancellation. The API key is removed from their environment.
  Ordinary user-entered programs inherit the user's environment and terminal.
- File reads are limited to 32 KiB. API requests have a 120-second deadline.
  Each request permits eight tool rounds followed by a summary request.
- **Commands are not sandboxed.** They run with your user permissions, including
  filesystem and network access. File reads and AI command output go to the API.
- No saved sessions, automatic context trimming, dedicated file-editing tool,
  indexing, plugins, or provider framework. Replies appear per API response;
  token streaming is deferred.

## Check

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
python3 tests/smoke.py
python3 tests/terminal.py
```

The tests cover parsing, directory changes, process output, limits, and the
whole shell/AI flow. The smoke test uses a local fake Responses server and needs
no key. The terminal test uses a pseudo-terminal to check interactive stdin and
Ctrl-C. These checks do not substitute for testing a real model.

`OPENAI_BASE_URL` optionally overrides `https://api.openai.com/v1`, primarily for
the local test server. It must expose `/responses`; Spark sends the configured
API key to that endpoint.
