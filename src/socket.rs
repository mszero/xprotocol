pub struct SocketPath {
    path: std::path::PathBuf,
    _temp_dir: Option<tempfile::TempDir>,
}

impl SocketPath {
    pub fn new(name: &str) -> Self {
        let temp_dir = tempfile::Builder::new()
            .prefix(name)
            .suffix(".sock")
            .tempdir()
            .expect("Failed to create temp dir");

        let path = temp_dir.path().join("socket.sock");

        Self {
            path,
            _temp_dir: Some(temp_dir),
        }
    }

    pub fn in_dir(mut self, dir: &std::path::Path) -> Self {
        self.path = dir.join(format!("{}.sock", uuid::Uuid::new_v4()));
        self
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn as_str(&self) -> String {
        self.path.to_string_lossy().to_string()
    }
}

impl Drop for SocketPath {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
