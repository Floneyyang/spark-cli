mod llm;
mod shell;
mod tools;

use anyhow::{Result, bail};
use rustyline::{DefaultEditor, error::ReadlineError};
use serde_json::{Value, json};
use std::{env, path::Path};

const MAX_TOOL_STEPS: usize = 8;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--help"] || args == ["-h"] {
        println!(
            "Spark — a small Rust shell with AI\n\nUsage: spark\n\nRun commands directly: ls, cd src, cargo test.\nEnter AI chat with ? or ? explain this project.\nKeep chatting without ?; /shell returns to shell mode.\nAI uses OPENAI_API_KEY and defaults to gpt-5.4-mini (override with OPENAI_MODEL).\nBuiltins: cd, pwd, exit. /help for more."
        );
        return Ok(());
    }
    if !args.is_empty() {
        bail!("unexpected arguments; use spark --help");
    }
    // Keep SIGINT handled even before the first command or API request, including
    // the brief gaps between readline calls. Ctrl-C must not terminate Spark.
    let _interrupts = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut ai = None;
    let mut root = env::current_dir()?.canonicalize()?;
    let mut editor = DefaultEditor::new()?;
    let mut history = Vec::new();
    let mut ai_mode = false;

    println!("Spark · Rust shell with AI\nDirectory: {}", root.display());
    println!("Type commands directly, or ? to enter AI chat. /shell returns to the shell.\n");

    loop {
        let prompt = if ai_mode {
            "ai> ".into()
        } else {
            format!(
                "spark:{}> ",
                tools::display_text(&root.display().to_string())
            )
        };
        let line = match editor.readline(&prompt) {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(error.into()),
        };
        let input = line.trim();
        match input {
            "" => continue,
            "/exit" | "/quit" => break,
            "/shell" => {
                ai_mode = false;
                println!("Shell mode. Type ? to resume AI chat.\n");
                continue;
            }
            "/clear" => {
                history.clear();
                println!("Conversation cleared.\n");
                continue;
            }
            "/help" | "help" if input == "/help" || !ai_mode => {
                println!(
                    "Shell commands: ls -la, cargo test, cd src, pwd, exit\nEnter AI chat: ? or ? Read Cargo.toml and explain this project\nIn AI mode, keep chatting normally without repeating ?.\n/shell  Return to shell mode (keeps AI history)\n/clear  Reset AI conversation (keeps the current mode)\n/exit   Quit Spark from either mode (or Ctrl-D)\n\nSpark launches programs directly and supports quoted arguments.\nPipes, redirection, expansion, chaining, and job control are not implemented yet.\nOnly AI-requested commands need approval. AI file reads and tool results go to the model.\nCtrl-C cancels the current action and keeps the current mode.\n"
                );
                continue;
            }
            _ => {}
        }
        editor.add_history_entry(input)?;
        let input = if let Some(request) = input.strip_prefix('?') {
            if !ai_mode {
                println!("AI mode. Keep chatting normally; /shell returns to the shell.");
            }
            ai_mode = true;
            request.trim()
        } else {
            input
        };
        if !ai_mode {
            let previous_root = root.clone();
            match shell::run(&mut root, input).await {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => eprintln!("spark: {}", tools::display_text(&format!("{error:#}"))),
            }
            if root != previous_root && !history.is_empty() {
                history.clear();
                println!("Directory changed; AI conversation cleared.");
            }
            continue;
        }
        if input.is_empty() {
            continue;
        }
        if ai.is_none() {
            match llm::Llm::from_env() {
                Ok(client) => {
                    println!("AI model: {}", client.model);
                    ai = Some(client);
                }
                Err(error) => {
                    eprintln!("AI unavailable: {error:#}");
                    continue;
                }
            }
        }
        history.push(json!({"role": "user", "content": input}));
        if let Err(error) = turn(ai.as_ref().unwrap(), &root, &mut history, &mut editor).await {
            eprintln!(
                "Error: {}\nYou can try again, or /clear to start fresh.",
                tools::display_text(&format!("{error:#}"))
            );
        }
        println!();
    }
    Ok(())
}

async fn turn(
    ai: &llm::Llm,
    root: &Path,
    history: &mut Vec<Value>,
    editor: &mut DefaultEditor,
) -> Result<()> {
    for step in 0..=MAX_TOOL_STEPS {
        println!("spark> Thinking…");
        let output = tokio::select! {
            result = ai.respond(history, root, step < MAX_TOOL_STEPS) => result?,
            _ = tokio::signal::ctrl_c() => bail!("request cancelled"),
        };
        // Preserve every output item, including reasoning, for the next request.
        history.extend(output.iter().cloned());
        let mut called_tool = false;
        let mut stopped = false;
        for item in output {
            match item["type"].as_str() {
                Some("message") => {
                    if let Some(parts) = item["content"].as_array() {
                        for part in parts {
                            if let Some(text) = part["text"].as_str().or(part["refusal"].as_str()) {
                                println!("spark> {}", tools::display_text(text));
                            }
                        }
                    }
                }
                Some("function_call") => {
                    called_tool = true;
                    let result = if stopped || step == MAX_TOOL_STEPS {
                        "Not executed: this turn has stopped or reached its tool limit.".into()
                    } else {
                        match execute(&item, root, editor).await {
                            Ok((result, cancelled)) => {
                                stopped = cancelled;
                                result
                            }
                            Err(error) => format!("Tool error: {error:#}"),
                        }
                    };
                    history.push(json!({
                        "type": "function_call_output",
                        "call_id": item["call_id"],
                        "output": result,
                    }));
                }
                _ => {}
            }
        }
        if stopped {
            println!("spark> Stopped. You can give me a different instruction.");
            return Ok(());
        }
        if !called_tool {
            return Ok(());
        }
    }
    println!("spark> Tool limit reached. Ask a follow-up to continue.");
    Ok(())
}

async fn execute(item: &Value, root: &Path, editor: &mut DefaultEditor) -> Result<(String, bool)> {
    let args: Value = serde_json::from_str(item["arguments"].as_str().unwrap_or(""))?;
    match item["name"].as_str() {
        Some("read_file") => {
            let path = args["path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing path"))?;
            println!("  read {}", tools::display_text(path));
            Ok((tools::read_file(root, path)?, false))
        }
        Some("run_command") => {
            let command = args["command"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing command"))?;
            tools::command_args(command)?;
            // Reject control characters so the approval always shows the actual command.
            if command
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
            {
                bail!("command contains terminal control characters");
            }
            println!("\n  Directory: {}\n  Command: {command}", root.display());
            if let Some(reason) = args["reason"].as_str() {
                println!("  Reason: {}", tools::display_text(reason));
            }
            let approved = match editor.readline("Run this command? [y/N] ") {
                Ok(answer) => matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
                Err(ReadlineError::Interrupted | ReadlineError::Eof) => false,
                Err(error) => return Err(error.into()),
            };
            if !approved {
                return Ok((
                    "User cancelled approval; command was not executed. Do not retry.".into(),
                    true,
                ));
            }
            let (result, cancelled) = tools::run_command(root, command).await?;
            println!("{}", tools::display_text(&result));
            Ok((result, cancelled))
        }
        _ => bail!("unknown tool"),
    }
}
