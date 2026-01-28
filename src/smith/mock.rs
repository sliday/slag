use std::future::Future;
use std::pin::Pin;

use super::Smith;
use crate::error::SlagError;

/// Mock smith for testing. Returns canned responses.
pub struct MockSmith {
    responses: Vec<String>,
    call_count: std::sync::atomic::AtomicUsize,
}

impl MockSmith {
    pub fn new(responses: Vec<String>) -> Self {
        Self {
            responses,
            call_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn fixed(response: &str) -> Self {
        Self::new(vec![response.to_string()])
    }

    pub fn failing() -> Self {
        Self::new(vec![])
    }

    pub fn call_count(&self) -> usize {
        self.call_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Smith for MockSmith {
    fn invoke(
        &self,
        _prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
        let idx = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if self.responses.is_empty() {
            return Box::pin(async {
                Err(SlagError::SmithFailed("mock smith: no responses".into()))
            });
        }

        let response = self.responses[idx % self.responses.len()].clone();
        Box::pin(async move { Ok(response) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixed_response() {
        let smith = MockSmith::fixed("Hello from mock");
        let result = smith.invoke("test prompt").await.unwrap();
        assert_eq!(result, "Hello from mock");
        assert_eq!(smith.call_count(), 1);
    }

    #[tokio::test]
    async fn cycling_responses() {
        let smith = MockSmith::new(vec!["first".into(), "second".into()]);
        assert_eq!(smith.invoke("a").await.unwrap(), "first");
        assert_eq!(smith.invoke("b").await.unwrap(), "second");
        assert_eq!(smith.invoke("c").await.unwrap(), "first");
    }

    #[tokio::test]
    async fn failing_smith() {
        let smith = MockSmith::failing();
        assert!(smith.invoke("test").await.is_err());
    }
}
