use anyhow::{Context, Result, bail};
use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::process::Command;

// A deliberately small command language: words, quotes, and backslash escapes.
// Reject unsupported syntax rather than silently giving it a different meaning.
pub fn parse_command(input: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c.is_control() && c != '\t' && c != '\n' {
            bail!("command contains control characters");
        }
        if quote == Some('\'') {
            if c == '\'' {
                quote = None;
            } else {
                word.push(c);
            }
            continue;
        }
        if c == '\\' {
            let next = chars.next().context("unfinished backslash escape")?;
            if next.is_control() {
                bail!("escaped control characters are not supported");
            }
            if quote == Some('"') && !matches!(next, '"' | '\\' | '$' | '`') {
                word.push('\\');
            }
            word.push(next);
            started = true;
            continue;
        }
        if quote == Some('"') {
            match c {
                '"' => quote = None,
                '$' | '`' => bail!(
                    "variable expansion and command substitution are not supported yet; use literal arguments"
                ),
                _ => word.push(c),
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                started = true;
            }
            ' ' | '\t' => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\n' | '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`' | '*' | '?' | '[' | ']'
            | '{' | '}' | '#' => {
                bail!(
                    "Spark currently supports one command with quoted arguments; pipes, redirection, chaining, expansion, and comments are not supported yet"
                );
            }
            _ => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        bail!("unclosed quote");
    }
    if started {
        words.push(word);
    }
    if words
        .first()
        .is_some_and(|word| word.is_empty() || word.contains('='))
    {
        bail!(
            "expected a program name; inline environment assignments are not supported (use the env program)"
        );
    }
    Ok(words)
}

pub fn change_directory(cwd: &Path, args: &[String]) -> Result<PathBuf> {
    if args.len() > 1 {
        bail!("usage: cd [directory]");
    }
    let target = args.first().map(String::as_str).unwrap_or("~");
    let target = if target == "~" || target.starts_with("~/") {
        let home = env::var_os("HOME").context("HOME is not set")?;
        PathBuf::from(home).join(target.strip_prefix("~/").unwrap_or(""))
    } else {
        cwd.join(target)
    };
    let target = target.canonicalize().context("cannot change directory")?;
    if !target.is_dir() {
        bail!("not a directory: {}", target.display());
    }
    Ok(target)
}

pub async fn run(cwd: &mut PathBuf, input: &str) -> Result<bool> {
    let args = parse_command(input)?;
    let Some(program) = args.first() else {
        return Ok(false);
    };
    match program.as_str() {
        "exit" if args.len() == 1 => return Ok(true),
        "exit" => bail!("usage: exit"),
        "cd" => *cwd = change_directory(cwd, &args[1..])?,
        "pwd" if args.len() == 1 => println!("{}", cwd.display()),
        "pwd" => bail!("usage: pwd"),
        _ => run_foreground(cwd, &args).await?,
    }
    Ok(false)
}

pub async fn run_foreground(cwd: &Path, args: &[String]) -> Result<()> {
    // Inherit the terminal so ordinary programs can read input and render output.
    // Spark waits while the child runs; there is no second shell interpreting it.
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .env("PWD", cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("could not start '{}'; check the command and PATH", args[0]))?;
    let status = tokio::select! {
        status = child.wait() => status?,
        _ = tokio::signal::ctrl_c() => {
            // Terminal Ctrl-C also reaches children in the foreground group.
            let _ = child.kill().await;
            println!();
            return Ok(());
        }
    };
    if !status.success() {
        eprintln!("spark: {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quotes_empty_arguments_and_escapes() {
        assert_eq!(
            parse_command(r#"printf '%s' "two words" '' escaped\ space"#).unwrap(),
            ["printf", "%s", "two words", "", "escaped space"]
        );
        assert_eq!(
            parse_command(r#"printf "%s\n" 'a|b;$HOME'"#).unwrap(),
            ["printf", "%s\\n", "a|b;$HOME"]
        );
        assert_eq!(
            parse_command(r#"echo one"two"'three'"#).unwrap(),
            ["echo", "onetwothree"]
        );
    }

    #[test]
    fn rejects_unsupported_or_incomplete_syntax() {
        for command in [
            "ls | wc",
            "echo hi > file",
            "ls && pwd",
            "echo $HOME",
            "echo *.rs",
            "echo `pwd`",
            "echo \"unfinished",
            "echo \\",
            "echo hello\npwd",
            "NAME=x env",
        ] {
            assert!(parse_command(command).is_err(), "accepted {command}");
        }
    }

    #[test]
    fn resolves_cd_without_changing_global_process_directory() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        std::fs::create_dir(cwd.join("two words")).unwrap();
        let next = change_directory(&cwd, &["two words".into()]).unwrap();
        assert_eq!(next, cwd.join("two words"));
        assert_eq!(change_directory(&next, &["..".into()]).unwrap(), cwd);
        assert!(change_directory(&cwd, &["missing".into()]).is_err());
    }
}
