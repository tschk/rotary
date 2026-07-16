//! LSP client manager stub.

pub struct LspManager {
    pub languages: Vec<String>,
}

impl LspManager {
    pub fn new() -> Self {
        Self {
            languages: Vec::new(),
        }
    }
    pub fn register(&mut self, language: impl Into<String>) {
        self.languages.push(language.into());
    }
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}
