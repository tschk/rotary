//! ACP (Agent Client Protocol) host stub.

pub struct AcpHost {
    pub running: bool,
}

impl AcpHost {
    pub fn new() -> Self {
        Self { running: false }
    }
    pub fn start(&mut self) {
        self.running = true;
    }
    pub fn stop(&mut self) {
        self.running = false;
    }
}

impl Default for AcpHost {
    fn default() -> Self {
        Self::new()
    }
}
