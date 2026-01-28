pub mod claude;
pub mod mock;

use std::future::Future;
use std::pin::Pin;

use crate::error::SlagError;

/// Async trait for invoking an AI smith (Claude or mock).
/// Uses boxed future for dyn compatibility.
pub trait Smith: Send + Sync {
    /// Send a prompt and receive the response text.
    fn invoke(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>>;
}

/// Check if response text contains unresolved questions
pub fn has_questions(text: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with('?') {
            return true;
        }
        if trimmed.starts_with("**Question")
            || trimmed.starts_with("Question")
            || trimmed.starts_with("Which ")
            || trimmed.starts_with("What ")
            || trimmed.starts_with("Should ")
            || trimmed.starts_with("Do you ")
            || trimmed.starts_with("Would you ")
            || trimmed.starts_with("Can you ")
            || trimmed.starts_with("Could you ")
        {
            return true;
        }
    }
    false
}

/// Self-iterate to resolve questions in smith output.
pub async fn self_iterate(
    smith: &dyn Smith,
    mut raw: String,
    max_iter: usize,
) -> Result<String, SlagError> {
    let mut iterations = 0;
    while has_questions(&raw) && iterations < max_iter {
        iterations += 1;
        let follow_up = format!(
            "{raw}\n\n---\n[SELF-QUERY RESOLUTION]\n\
            You asked questions above. You are the expert. Answer them yourself:\n\
            - Make decisive choices based on best practices\n\
            - Choose the most sensible option when uncertain\n\
            - Do not ask for clarification - decide and proceed\n\n\
            Now output the COMPLETE deliverable with all decisions made.\n\
            NO QUESTIONS. NO PREAMBLE. Just the final output."
        );
        raw = smith.invoke(&follow_up).await?;
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_questions() {
        assert!(has_questions("What framework should we use?"));
        assert!(has_questions("**Question**: which approach?"));
        assert!(has_questions("Should we use React or Vue?"));
        assert!(!has_questions("# Blueprint\nThis is a plan."));
        assert!(!has_questions("Create the file structure."));
    }
}
