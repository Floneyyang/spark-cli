mod llm;
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
            "Spark — a small AI assistant for your terminal\n\nUsage: spark\n\nSet OPENAI_API_KEY and OPENAI_MODEL, then launch in your project.\nCommands: /help, /clear, /exit"
        );
        return Ok(());
    }
    if !args.is_empty() {
        bail!("unexpected arguments; use spark --help");
    }
    let ai = llm::Llm::from_env()?;
    let root = env::current_dir()?.canonicalize()?;
    let mut editor = DefaultEditor::new()?;
    let mut history = Vec::new();

    println!("Spark · {}\nProject: {}", ai.model, root.display());
    println!("Ask about your project or request a task. /help for commands.");
    println!("Project file reads go to the model. Every shell command needs approval.\n");

    loop {
        let line = match editor.readline("you> ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(error.into()),
        };
        let input = line.trim();
        match input {
            "" => continue,
            "/exit" | "/quit" => break,
            "/clear" => {
                history.clear();
                println!("Conversation cleared.\n");
                continue;
            }
            "/help" => {
                println!(
                    "Ask a question or give a task, e.g. 'Read Cargo.toml and explain this project'.\n/clear  Reset conversation\n/exit   Quit (or Ctrl-D)\n\nCommands need y/yes to run. Enter or Ctrl-C cancels approval.\nCtrl-C cancels a running command or API request.\n"
                );
                continue;
            }
            _ if input.starts_with('/') => {
                println!("Unknown command. Type /help.\n");
                continue;
            }
            _ => {}
        }
        editor.add_history_entry(input)?;
        history.push(json!({"role": "user", "content": input}));
        if let Err(error) = turn(&ai, &root, &mut history, &mut editor).await {
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
