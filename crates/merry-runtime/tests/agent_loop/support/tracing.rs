use std::{
    future::Future,
    sync::{Arc, Mutex, OnceLock},
};

fn trace_output_buffer() -> &'static Arc<Mutex<Vec<u8>>> {
    #[derive(Clone)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("buffer mutex should not be poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    static TRACE_OUTPUT: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    TRACE_OUTPUT.get_or_init(|| {
        use tracing_subscriber::{fmt, prelude::*};

        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer_bytes = Arc::clone(&bytes);
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .json()
                .with_writer(move || Buffer(Arc::clone(&writer_bytes))),
        );
        tracing::subscriber::set_global_default(subscriber)
            .expect("test tracing subscriber should install once");
        bytes
    })
}

static TRACE_CAPTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn capture_traces_for<F, R>(trace_marker: &str, future: F) -> (R, String)
where
    F: Future<Output = R>,
{
    let _capture_guard = TRACE_CAPTURE_LOCK.lock().await;
    let bytes = Arc::clone(trace_output_buffer());
    let start = bytes
        .lock()
        .expect("buffer mutex should not be poisoned")
        .len();
    let result = future.await;
    let text = {
        let guard = bytes.lock().expect("buffer mutex should not be poisoned");
        String::from_utf8(guard[start..].to_vec()).expect("trace output should be UTF-8")
    };
    let text = text
        .lines()
        .filter(|line| line.contains(trace_marker))
        .collect::<Vec<_>>()
        .join("\n");
    (result, text)
}
