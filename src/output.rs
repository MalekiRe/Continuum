use std::fmt;
use std::sync::Arc;

#[derive(Clone)]
pub struct OutputSink(Arc<dyn Fn(&str) + Send + Sync>);

impl OutputSink {
    pub fn new(write: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(write))
    }

    pub fn write(&self, text: &str) {
        (self.0)(text);
    }
}

impl Default for OutputSink {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

impl fmt::Debug for OutputSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OutputSink(..)")
    }
}
