use std::future::Future;
use std::pin::Pin;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::Smith;
use crate::config::SmithConfig;
use crate::error::SlagError;

/// Claude CLI smith that spawns `claude -p` as a subprocess.
pub struct ClaudeSmith {
    command: String,
}

impl ClaudeSmith {
    pub fn new(command: String) -> Self {
        Self { command }
    }

    pub fn from_config(config: &SmithConfig, skill: &str, grade: u8) -> Self {
        Self {
            command: config.select(skill, grade).to_string(),
        }
    }

    pub fn plan(config: &SmithConfig) -> Self {
        Self {
            command: config.plan.clone(),
        }
    }

    pub fn base(config: &SmithConfig) -> Self {
        Self {
            command: config.base.clone(),
        }
    }

    async fn invoke_impl(&self, prompt: &str) -> Result<String, SlagError> {
        let parts: Vec<&str> = shell_words(&self.command);
        if parts.is_empty() {
            return Err(SlagError::SmithFailed("empty smith command".into()));
        }

        let program = parts[0];
        let args = &parts[1..];

        let mut child = Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| SlagError::SmithFailed(format!("failed to spawn {program}: {e}")))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| SlagError::SmithFailed(format!("stdin write failed: {e}")))?;
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| SlagError::SmithFailed(format!("wait failed: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SlagError::SmithFailed(format!(
                "exit {}: {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

impl Smith for ClaudeSmith {
    fn invoke(&self, prompt: &str) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
        let prompt = prompt.to_string();
        Box::pin(async move { self.invoke_impl(&prompt).await })
    }
}

/// Simple shell word splitting (handles single/double quotes).
fn shell_words(s: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut i = 0;
    let bytes = s.as_bytes();
    let len = bytes.len();

    while i < len {
        while i < len && bytes[i] == b' ' {
            i += 1;
        }
        if i >= len {
            break;
        }

        if bytes[i] == b'\'' {
            i += 1;
            let start = i;
            while i < len && bytes[i] != b'\'' {
                i += 1;
            }
            words.push(&s[start..i]);
            if i < len {
                i += 1;
            }
        } else if bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < len && bytes[i] != b'"' {
                i += 1;
            }
            words.push(&s[start..i]);
            if i < len {
                i += 1;
            }
        } else {
            let start = i;
            while i < len && bytes[i] != b' ' {
                i += 1;
            }
            words.push(&s[start..i]);
        }
    }

    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_basic() {
        let words = shell_words("claude --dangerously-skip-permissions -p");
        assert_eq!(
            words,
            vec!["claude", "--dangerously-skip-permissions", "-p"]
        );
    }

    #[test]
    fn shell_words_quoted() {
        let words = shell_words("claude -p --allowedTools 'Bash Edit Read'");
        assert_eq!(
            words,
            vec!["claude", "-p", "--allowedTools", "Bash Edit Read"]
        );
    }

    #[test]
    fn shell_words_double_quoted() {
        let words = shell_words(r#"claude -p --allowedTools "Bash Edit Read""#);
        assert_eq!(
            words,
            vec!["claude", "-p", "--allowedTools", "Bash Edit Read"]
        );
    }
}
