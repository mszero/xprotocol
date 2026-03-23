use futures::Future;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeType {
    #[default]
    SingleThread,
    MultiThreadWorkStealing,
    MultiThreadNoWorkStealing,
}

impl RuntimeType {
    pub fn recommended() -> Self {
        Self::MultiThreadNoWorkStealing
    }
}

pub struct ProtocolRuntime {
    runtime_type: RuntimeType,
    threads: Option<usize>,
}

impl ProtocolRuntime {
    pub fn new() -> Self {
        Self {
            runtime_type: RuntimeType::default(),
            threads: None,
        }
    }

    pub fn with_type(mut self, runtime_type: RuntimeType) -> Self {
        self.runtime_type = runtime_type;
        self
    }

    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = Some(threads);
        self
    }

    fn get_thread_count(&self) -> usize {
        self.threads.unwrap_or_else(num_cpus::get)
    }

    pub fn run<F: Future>(self, f: F) -> F::Output
    where
        F: Future,
    {
        match self.runtime_type {
            RuntimeType::SingleThread => {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to build single-threaded runtime");
                rt.block_on(f)
            }
            RuntimeType::MultiThreadWorkStealing => {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to build multi-threaded runtime");
                rt.block_on(f)
            }
            RuntimeType::MultiThreadNoWorkStealing => {
                let threads = self.get_thread_count();
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(threads)
                    .enable_all()
                    .build()
                    .expect("Failed to build multi-threaded no-work-stealing runtime");
                rt.block_on(f)
            }
        }
    }

    pub async fn run_async<F: Future>(self, f: F) -> F::Output
    where
        F: Future,
    {
        self.run(f)
    }
}

impl Default for ProtocolRuntime {
    fn default() -> Self {
        Self::new()
    }
}

pub fn current_handle() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

pub fn create_runtime(
    runtime_type: RuntimeType,
    threads: Option<usize>,
) -> tokio::runtime::Runtime {
    match runtime_type {
        RuntimeType::SingleThread => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to build single-threaded runtime"),
        RuntimeType::MultiThreadWorkStealing => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to build multi-threaded runtime"),
        RuntimeType::MultiThreadNoWorkStealing => {
            let threads = threads.unwrap_or_else(num_cpus::get);
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(threads)
                .enable_all()
                .build()
                .expect("Failed to build multi-threaded runtime")
        }
    }
}
