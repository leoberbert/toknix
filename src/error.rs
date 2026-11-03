use std::fmt;

#[derive(Debug)]
pub struct Failure {
    message: String,
    auth_rejected: bool,
}

impl Failure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            auth_rejected: false,
        }
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            auth_rejected: true,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn is_auth(&self) -> bool {
        self.auth_rejected
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

pub type ProviderResult<T> = Result<T, Failure>;
