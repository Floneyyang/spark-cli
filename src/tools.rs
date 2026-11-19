use anyhow::{Context, Result, bail};
use std::{fs::File, io::Read, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

const OUTPUT_LIMIT: usize = 32 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

pub fn read_file(root: &Path, path: &str) -> Result<String> {
    let path = root
        .join(path)
        .canonicalize()
        .context("cannot resolve file")?;
    if !path.starts_with(root) {
        bail!("file is outside the project directory");
    }
    if !path.is_file() {
        bail!("path is not a regular file");
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > OUTPUT_LIMIT;
    bytes.truncate(OUTPUT_LIMIT);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if text.contains('\0') {
        bail!("file appears to be binary");
    }
    if truncated {
        text.push_str("\n[truncated at 32 KiB]");
    }
    Ok(text)
}

// The shell and its children share a process group. Always clean it up, including
// when the future is cancelled or the shell leaves a background process running.
struct ProcessGroup(u32);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: a negative PID addresses the group created for this child only.
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}

pub async fn run_command(root: &Path, command: &str) -> Result<(String, bool)> {
    run_with_timeout(root, command, COMMAND_TIMEOUT).await
}

async fn run_with_timeout(root: &Path, command: &str, timeout: Duration) -> Result<(String, bool)> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("could not start command")?;
    let mut group = Some(ProcessGroup(child.id().context("missing command PID")?));
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_buffer, mut err_buffer) = ([0; 4096], [0; 4096]);
    let (mut out_open, mut err_open) = (true, true);
    let mut status = None;
    let mut cancelled = false;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    let outcome = loop {
        if let Some(status) = status
            && !out_open
            && !err_open
        {
            break format!("Exit: {status}");
        }
        tokio::select! {
            result = child.wait(), if status.is_none() => {
                status = Some(result?);
                // Close pipes held by background children after the shell exits.
                drop(group.take());
            }
            result = stdout.read(&mut out_buffer), if out_open => {
                let count = result?;
                out_open = count != 0;
                out.extend_from_slice(&out_buffer[..count]);
                if out.len() > OUTPUT_LIMIT {
                    break "Stopped: stdout exceeded 32 KiB (truncated)".into();
                }
            }
            result = stderr.read(&mut err_buffer), if err_open => {
                let count = result?;
                err_open = count != 0;
                err.extend_from_slice(&err_buffer[..count]);
                if err.len() > OUTPUT_LIMIT {
                    break "Stopped: stderr exceeded 32 KiB (truncated)".into();
                }
            }
            _ = &mut deadline => break "Stopped: command timed out".into(),
            _ = tokio::signal::ctrl_c() => {
                cancelled = true;
                break "Stopped: cancelled by user".into();
            }
        }
    };
    drop(group);
    let _ = child.wait().await;
    out.truncate(OUTPUT_LIMIT);
    err.truncate(OUTPUT_LIMIT);
    Ok((
        format!(
            "{outcome}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        ),
        cancelled,
    ))
}

pub fn display_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn reads_project_files_but_rejects_escaping_symlinks() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        fs::write(root.join("hello.txt"), "hello").unwrap();
        fs::write(outside.path().join("secret"), "outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        assert_eq!(read_file(&root, "hello.txt").unwrap(), "hello");
        assert!(read_file(&root, "link/secret").is_err());
        assert!(read_file(&root, ".").is_err());
    }

    #[test]
    fn large_files_are_truncated_without_breaking_unicode() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        fs::write(root.join("large"), "🦀".repeat(OUTPUT_LIMIT)).unwrap();
        let text = read_file(&root, "large").unwrap();
        assert!(text.ends_with("[truncated at 32 KiB]"));
        assert!(text.len() < OUTPUT_LIMIT + 100);
    }

    #[tokio::test]
    async fn captures_both_streams_and_failure_status() {
        let project = tempfile::tempdir().unwrap();
        let (result, cancelled) =
            run_command(project.path(), "printf hello; printf problem >&2; exit 7")
                .await
                .unwrap();
        assert!(result.contains("exit status: 7"));
        assert!(result.contains("stdout:\nhello"));
        assert!(result.contains("stderr:\nproblem"));
        assert!(!cancelled);
    }

    #[tokio::test]
    async fn times_out_even_if_a_pipe_closes_early() {
        let project = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let (result, _) = run_with_timeout(
            project.path(),
            "exec 1>&-; sleep 10",
            Duration::from_millis(100),
        )
        .await
        .unwrap();
        assert!(result.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn bounds_output_and_does_not_wait_for_background_children() {
        let project = tempfile::tempdir().unwrap();
        let (result, _) = run_command(project.path(), "while :; do printf 'lots of output'; done")
            .await
            .unwrap();
        assert!(result.contains("truncated"));
        assert!(result.len() < OUTPUT_LIMIT + 200);
        let started = std::time::Instant::now();
        let (result, _) = run_command(project.path(), "sleep 10 & printf done")
            .await
            .unwrap();
        assert!(result.contains("done"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
