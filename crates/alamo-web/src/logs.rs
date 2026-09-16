//! A bounded in-memory copy of the daemon's log, so the dashboard can offer it as a
//! download without a writable log directory. The daemon still logs to stdout.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Default capacity: about 2 MB of formatted lines.
pub const DEFAULT_CAPACITY: usize = 2 * 1024 * 1024;

/// Shared handle to the ring of recent log lines.
#[derive(Clone, Debug)]
pub struct LogBuffer {
    inner: Arc<Mutex<Ring>>,
}

#[derive(Debug)]
struct Ring {
    lines: VecDeque<String>,
    bytes: usize,
    capacity: usize,
    /// Lines evicted since start, so the download can say how much is missing.
    evicted: u64,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl LogBuffer {
    /// A buffer that keeps at most `capacity` bytes of lines.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Ring {
                lines: VecDeque::new(),
                bytes: 0,
                capacity,
                evicted: 0,
            })),
        }
    }

    /// The tracing layer that feeds this buffer. Install it next to the stdout layer.
    pub fn layer(&self) -> LogLayer {
        LogLayer {
            buffer: self.clone(),
        }
    }

    /// Append one formatted line, evicting the oldest lines to stay within capacity.
    pub fn push(&self, line: String) {
        let Ok(mut ring) = self.inner.lock() else {
            return;
        };
        let len = line.len() + 1;
        if len > ring.capacity {
            return;
        }
        while ring.bytes + len > ring.capacity {
            if let Some(old) = ring.lines.pop_front() {
                ring.bytes -= old.len() + 1;
                ring.evicted += 1;
            } else {
                break;
            }
        }
        ring.bytes += len;
        ring.lines.push_back(line);
    }

    /// Everything currently held, oldest first, one line each, with a note at the top
    /// when older lines have already been dropped.
    pub fn contents(&self) -> String {
        let Ok(ring) = self.inner.lock() else {
            return String::new();
        };
        let mut out = String::with_capacity(ring.bytes + 96);
        if ring.evicted > 0 {
            let _ = writeln!(
                out,
                "# {} earlier lines were dropped; this buffer keeps the last {} bytes",
                ring.evicted, ring.capacity
            );
        }
        for line in &ring.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// Number of lines held.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|r| r.lines.len()).unwrap_or(0)
    }

    /// Whether nothing has been logged yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The `tracing_subscriber` layer behind [`LogBuffer::layer`].
pub struct LogLayer {
    buffer: LogBuffer,
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut line = String::with_capacity(160);
        let _ = write!(
            line,
            "{} {:>5} ",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            meta.level()
        );
        let mut visitor = LineVisitor {
            line: &mut line,
            fields: String::new(),
        };
        event.record(&mut visitor);
        if !visitor.fields.is_empty() {
            let fields = std::mem::take(&mut visitor.fields);
            line.push_str(&fields);
        }
        self.buffer.push(line);
    }
}

/// Writes the `message` field first, then `key=value` pairs in the order recorded.
struct LineVisitor<'a> {
    line: &'a mut String,
    fields: String,
}

impl LineVisitor<'_> {
    fn field(&mut self, field: &Field, value: std::fmt::Arguments<'_>) {
        if field.name() == "message" {
            let _ = self.line.write_fmt(value);
        } else {
            let _ = write!(self.fields, " {}={}", field.name(), value);
        }
    }
}

impl Visit for LineVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.field(field, format_args!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.line.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.field(field, format_args!("{value}"));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.field(field, format_args!("{value}"));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.field(field, format_args!("{value}"));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.field(field, format_args!("{value}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn keeps_the_newest_lines_within_capacity() {
        let buffer = LogBuffer::with_capacity(40);
        for i in 0..10 {
            buffer.push(format!("line {i:02}xxxxxxx")); // 14 bytes + newline
        }
        let text = buffer.contents();
        assert!(text.starts_with("# 8 earlier lines were dropped"), "{text}");
        assert!(text.ends_with("line 08xxxxxxx\nline 09xxxxxxx\n"), "{text}");
        assert_eq!(buffer.len(), 2);
        buffer.push("x".repeat(100));
        assert_eq!(buffer.len(), 2, "an oversized line is ignored");
    }

    #[test]
    fn layer_formats_events_with_message_first() {
        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::registry().with(buffer.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(coin = "LTC", height = 7u64, "node connected");
            tracing::warn!(err = %"boom", "node unreachable");
        });
        let text = buffer.contents();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].contains(" INFO node connected coin=\"LTC\" height=7"),
            "{text}"
        );
        assert!(
            lines[1].contains(" WARN node unreachable err=boom"),
            "{text}"
        );
        assert!(lines[0].ends_with('7') && lines[0].len() > 24);
    }
}
