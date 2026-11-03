pub mod chrome;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod deepseek;
pub mod kiro;
pub mod mimo;
pub mod openai;

use std::sync::Mutex;

#[derive(Default)]
pub struct MemoryCache {
    cursor_access_token: Mutex<Option<String>>,
}

impl MemoryCache {
    pub fn cached_cursor_token(&self) -> Option<String> {
        self.cursor_access_token
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn store_cursor_token(&self, token: String) {
        *self
            .cursor_access_token
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(token);
    }
}
