use std::backtrace::Backtrace;
use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};

use tracing::field::{Field, Visit};
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::fmt::format::{FormatFields, Writer};
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, fmt as tracing_fmt};

mod stderr;
/// Drains persistent logs before releasing the bounded stderr shutdown guard.
pub struct StderrGuard {
    _persistent: PersistentGuard,
    _stderr: stderr::Guard,
}

struct PersistentGuard(Arc<DiagnosticState>, Option<std::thread::JoinHandle<()>>);

impl Drop for PersistentGuard {
    fn drop(&mut self) {
        self.0
            .output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .closed = true;
        self.0.ready.notify_all();
        if let Some(worker) = self.1.take() {
            let _ = worker.join();
        }
    }
}

struct DiagnosticState {
    output: Mutex<DiagnosticOutput>,
    ready: Condvar,
    drained: Condvar,
}

impl DiagnosticState {
    fn new(log_dir: &Path) -> (Arc<Self>, PersistentGuard) {
        let (output, file) = DiagnosticOutput::new(log_dir);
        let state = Arc::new(Self {
            output: Mutex::new(output),
            ready: Condvar::new(),
            drained: Condvar::new(),
        });
        let worker_state = Arc::clone(&state);
        let worker = std::thread::Builder::new()
            .name("diagnostic-file".into())
            .spawn(move || worker_state.run(file))
            .expect("start diagnostic file writer");
        let guard = PersistentGuard(Arc::clone(&state), Some(worker));
        (state, guard)
    }

    fn append(&self, text: String, parts: Option<Parts>) {
        let mut output = self.output.lock().unwrap_or_else(|p| p.into_inner());
        if !output.closed {
            output.append(text, parts);
            self.ready.notify_one();
        }
    }

    fn flush(&self) {
        let mut output = self.output.lock().unwrap_or_else(|p| p.into_inner());
        while output.written < output.end() {
            output = self.drained.wait(output).unwrap_or_else(|p| p.into_inner());
        }
    }

    fn run(&self, mut file: Option<RotatingLog>) {
        loop {
            let update = {
                let mut output = self.output.lock().unwrap_or_else(|p| p.into_inner());
                while output.written == output.end() && !output.closed {
                    output = self.ready.wait(output).unwrap_or_else(|p| p.into_inner());
                }
                if output.written == output.end() {
                    return;
                }
                if output.written < output.start_bytes {
                    output.append(OMITTED_RECORDS.into(), None);
                }
                (output.text(output.written), output.end())
            };
            if let Some(log) = &mut file
                && let Err(error) = update
                    .0
                    .split_inclusive('\n')
                    .try_for_each(|line| log.append(line.as_bytes()))
            {
                file = None;
                self.output
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .append(
                        sanitize_free_text(&format!(
                            "WARN rufin::diagnostics: local log file is unavailable: {error}\n"
                        )),
                        None,
                    );
            }
            self.output
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .written = update.1;
            self.drained.notify_all();
        }
    }
}

const BUFFER_MAX_BYTES: usize = 2 * 1024 * 1024;
const OMITTED_RECORDS: &str =
    "WARN rufin::diagnostics: older log records omitted while the file writer was busy\n";
const LOG_SEGMENT_MAX_BYTES: u64 = 2 * 1024 * 1024;
const LOG_SEGMENTS: usize = 3;
const LOG_FILE: &str = "rufin.log";
const PANIC_FILE: &str = "rufin-panic.log";

const NORMAL_FILTER: &str = concat!(
    "warn,",
    "metadata_lookup=info,",
    "artwork=info,",
    "desktop_integration=info,",
    "library=info,",
    "lyrics=info,",
    "playback=info,",
    "playback_gstreamer=info,",
    "rufin=info,",
    "rufin_core::downloads=warn,",
    "secrets=info,",
    "sources=info,",
    "ui=info,",
    "lofty=error"
);
const DEBUG_FILTER: &str = concat!(
    "warn,",
    "metadata_lookup=debug,",
    "artwork=debug,",
    "desktop_integration=debug,",
    "library=debug,",
    "lyrics=debug,",
    "playback=debug,",
    "playback_gstreamer=debug,",
    "rufin=debug,",
    "rufin_core::downloads=warn,",
    "secrets=debug,",
    "sources=debug,",
    "ui=debug,",
    "lofty=error"
);

type FilterHandle = reload::Handle<EnvFilter, Registry>;

pub struct Diagnostics {
    output: Arc<DiagnosticState>,
    filter: FilterHandle,
    debug_enabled: AtomicBool,
}

impl Diagnostics {
    pub fn install(state_dir: PathBuf) -> (Arc<Self>, StderrGuard) {
        let log_dir = state_dir.join("logs");
        let (output, persistent) = DiagnosticState::new(&log_dir);
        let debug_enabled = startup_debug_enabled();
        let filter = startup_filter(debug_enabled);
        let (filter_layer, filter_handle) = reload::Layer::new(filter);
        let stored = StoredLayer {
            output: Arc::clone(&output),
        };
        let (stderr, stderr_guard) = stderr::Writer::new(std::io::stderr());
        let terminal = tracing_fmt::layer()
            .compact()
            .with_ansi(std::io::stderr().is_terminal())
            .fmt_fields(PrivacyFields)
            .with_writer(stderr.clone());
        Registry::default()
            .with(filter_layer)
            .with(terminal)
            .with(stored)
            .try_init()
            .expect("install Rufin diagnostics subscriber");

        install_panic_hook(log_dir, stderr);
        install_glib_log_capture(Arc::clone(&output));
        (
            Arc::new(Self {
                output,
                filter: filter_handle,
                debug_enabled: AtomicBool::new(debug_enabled),
            }),
            StderrGuard {
                _persistent: persistent,
                _stderr: stderr_guard,
            },
        )
    }
}

impl Diagnostics {
    pub fn debug_enabled(&self) -> bool {
        self.debug_enabled.load(Ordering::Relaxed)
    }

    pub fn set_debug_enabled(&self, enabled: bool) -> Result<(), String> {
        self.filter
            .reload(profile_filter(enabled))
            .map_err(|error| format!("could not change debug logging: {error}"))?;
        self.debug_enabled.store(enabled, Ordering::Relaxed);
        tracing::info!(debug = enabled, "diagnostic logging changed");
        Ok(())
    }

    pub fn snapshot(&self) -> String {
        self.output
            .output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .text(0)
    }

    pub fn updates(&self, cursor: u64) -> DiagnosticUpdate {
        self.output
            .output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .updates(cursor)
    }
}

fn install_glib_log_capture(output: Arc<DiagnosticState>) {
    use glib::translate::IntoGlib as _;

    // Older supported GLib versions expose the fatal mask only through this setter.
    let fatal = glib::log_set_always_fatal(glib::LogLevels::LEVEL_ERROR);
    glib::log_set_always_fatal(fatal);
    let record = move |domain: Option<&str>, level: glib::LogLevel, message: &str| {
        record_glib_log(domain, level, message);
        if fatal.bits() & level.into_glib() != 0 {
            output.flush();
        }
    };
    glib::log_set_default_handler(record.clone());
    glib::log_set_writer_func(move |level, fields| {
        let field = |key| {
            fields
                .iter()
                .find(|field| field.key() == key)
                .and_then(glib::LogField::value_str)
        };
        record(
            field("GLIB_DOMAIN"),
            level,
            field("MESSAGE").unwrap_or_default(),
        );
        if fatal.bits() & level.into_glib() != 0 {
            std::process::abort();
        }
        glib::LogWriterOutput::Handled
    });
}

fn record_glib_log(domain: Option<&str>, level: glib::LogLevel, message: &str) {
    let domain = domain.unwrap_or("GLib");
    match level {
        glib::LogLevel::Error | glib::LogLevel::Critical => {
            tracing::error!(target: "rufin::native", domain, "{message}")
        }
        glib::LogLevel::Warning => tracing::warn!(target: "rufin::native", domain, "{message}"),
        glib::LogLevel::Message | glib::LogLevel::Info => {
            tracing::info!(target: "rufin::native", domain, "{message}")
        }
        glib::LogLevel::Debug
            if domain == "Gtk" && message.starts_with("snapshot symbolic icon ") =>
        {
            tracing::trace!(target: "rufin::native", domain, "{message}")
        }
        glib::LogLevel::Debug => tracing::debug!(target: "rufin::native", domain, "{message}"),
    }
}

fn startup_debug_enabled() -> bool {
    std::env::var("RUST_LOG").is_ok_and(|value| value.trim().eq_ignore_ascii_case("debug"))
}

fn startup_filter(debug_enabled: bool) -> EnvFilter {
    let Some(custom) = std::env::var("RUST_LOG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .filter(|value| !value.trim().eq_ignore_ascii_case("debug"))
    else {
        return profile_filter(debug_enabled);
    };
    EnvFilter::try_new(custom).unwrap_or_else(|_| profile_filter(debug_enabled))
}

fn profile_filter(debug_enabled: bool) -> EnvFilter {
    EnvFilter::new(if debug_enabled {
        DEBUG_FILTER
    } else {
        NORMAL_FILTER
    })
}

/// The log's entries after a cursor. `start` is where the kept log begins: entries a client holds
/// from before it are gone.
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DiagnosticUpdate {
    pub end: u64,
    pub start: u64,
    pub entries: Vec<DiagnosticEntry>,
}

/// One record of the log. Text that Rufin did not log itself, such as an earlier crash report, has
/// no level: it is all in `message`.
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DiagnosticEntry {
    pub position: u64,
    pub time_millis: u64,
    pub level: Option<DiagnosticLevel>,
    pub origin: String,
    pub message: String,
    pub fields: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DiagnosticLevel {
    Error,
    Warning,
    Info,
    Debug,
    Trace,
}

/// Where a logged record's parts are in its line: `time level origin: message fields`.
#[derive(Clone, Copy)]
struct Parts {
    time_millis: u64,
    level: DiagnosticLevel,
    origin: usize,
    origin_end: usize,
    message_end: usize,
}

#[derive(Default)]
struct DiagnosticOutput {
    entries: VecDeque<(usize, Option<Parts>)>,
    // Keep history together instead of retaining allocations from playback and decode workers.
    bytes: VecDeque<u8>,
    start_bytes: u64,
    written: u64,
    closed: bool,
}

impl DiagnosticOutput {
    fn new(log_dir: &Path) -> (Self, Option<RotatingLog>) {
        let mut output = Self::default();
        let mut file = match RotatingLog::open(log_dir) {
            Ok(file) => Some(file),
            Err(error) => {
                output.append(
                    sanitize_free_text(&format!(
                        "WARN rufin::diagnostics: local log file is unavailable: {error}\n"
                    )),
                    None,
                );
                None
            }
        };
        import_previous_panic_report(log_dir, &mut output);
        if let Some(log) = &mut file
            && let Err(error) = log.append(output.text(0).as_bytes())
        {
            output.append(
                sanitize_free_text(&format!(
                    "WARN rufin::diagnostics: local log file is unavailable: {error}\n"
                )),
                None,
            );
            file = None;
        }
        output.written = output.end();
        (output, file)
    }

    #[expect(
        clippy::string_slice,
        reason = "the buffer boundary is advanced to a UTF-8 character boundary before slicing"
    )]
    fn append(&mut self, mut text: String, mut parts: Option<Parts>) {
        if text.len() > BUFFER_MAX_BYTES {
            let mut start = text.len() - BUFFER_MAX_BYTES;
            while !text.is_char_boundary(start) {
                start += 1;
            }
            text = text[start..].to_string();
            parts = None;
        }
        while self.bytes.len() + text.len() > BUFFER_MAX_BYTES {
            let Some((bytes, _)) = self.entries.pop_front() else {
                break;
            };
            self.bytes.drain(..bytes);
            self.start_bytes += bytes as u64;
        }
        self.bytes.extend(text.bytes());
        self.entries.push_back((text.len(), parts));
    }

    fn end(&self) -> u64 {
        self.start_bytes + self.bytes.len() as u64
    }

    /// The log's text from a cursor, as the log file holds it.
    fn text(&self, cursor: u64) -> String {
        let skip = cursor
            .saturating_sub(self.start_bytes)
            .min(self.bytes.len() as u64) as usize;
        String::from_utf8(self.bytes.range(skip..).copied().collect())
            .expect("diagnostic cursors end at UTF-8 boundaries")
    }

    #[expect(
        clippy::string_slice,
        reason = "a record's parts end where the layer that wrote the line pushed ASCII separators"
    )]
    fn updates(&self, cursor: u64) -> DiagnosticUpdate {
        let mut entries = Vec::new();
        let mut offset = 0;
        for (bytes, parts) in &self.entries {
            let position = self.start_bytes + offset as u64;
            let record = offset..offset + bytes;
            offset = record.end;
            if position < cursor {
                continue;
            }
            let text = String::from_utf8(self.bytes.range(record).copied().collect())
                .expect("diagnostic records end at UTF-8 boundaries");
            entries.push(match parts {
                Some(parts) => DiagnosticEntry {
                    position,
                    time_millis: parts.time_millis,
                    level: Some(parts.level),
                    origin: text[parts.origin..parts.origin_end].to_string(),
                    message: text[parts.origin_end + 2..parts.message_end].to_string(),
                    fields: text[parts.message_end..].trim().to_string(),
                },
                None => DiagnosticEntry {
                    position,
                    time_millis: 0,
                    level: None,
                    origin: String::new(),
                    message: text.trim_end().to_string(),
                    fields: String::new(),
                },
            });
        }
        DiagnosticUpdate {
            end: self.end(),
            start: self.start_bytes,
            entries,
        }
    }
}

fn import_previous_panic_report(log_dir: &Path, output: &mut DiagnosticOutput) {
    let path = log_dir.join(PANIC_FILE);
    let Ok(report) = fs::read_to_string(&path) else {
        return;
    };
    output.append(
        format!(
            "Previous Rufin crash report:\n{}\n",
            sanitize_free_text(&report)
        ),
        None,
    );
    let _ = fs::remove_file(path);
}

struct RotatingLog {
    directory: PathBuf,
    file: Option<File>,
    bytes: u64,
}

impl RotatingLog {
    fn open(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        rotate_files(directory, LOG_FILE)?;
        let path = directory.join(LOG_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            file: Some(file),
            bytes: 0,
        })
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write as _;

        if self.bytes > 0 && self.bytes.saturating_add(bytes.len() as u64) > LOG_SEGMENT_MAX_BYTES {
            if let Some(mut file) = self.file.take() {
                file.flush()?;
            }
            rotate_files(&self.directory, LOG_FILE)?;
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(self.directory.join(LOG_FILE))?,
            );
            self.bytes = 0;
        }
        let bytes = if bytes.len() as u64 > LOG_SEGMENT_MAX_BYTES {
            &bytes[bytes.len() - LOG_SEGMENT_MAX_BYTES as usize..]
        } else {
            bytes
        };
        let Some(file) = self.file.as_mut() else {
            return Err(io::Error::other("log file is unavailable"));
        };
        file.write_all(bytes)?;
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        Ok(())
    }
}

fn rotate_files(directory: &Path, base: &str) -> io::Result<()> {
    let oldest = directory.join(format!("{base}.{}", LOG_SEGMENTS - 1));
    if oldest.exists() {
        fs::remove_file(oldest)?;
    }
    for index in (1..LOG_SEGMENTS - 1).rev() {
        let from = directory.join(format!("{base}.{index}"));
        if from.exists() {
            fs::rename(from, directory.join(format!("{base}.{}", index + 1)))?;
        }
    }
    let current = directory.join(base);
    if current.exists() {
        fs::rename(current, directory.join(format!("{base}.1")))?;
    }
    Ok(())
}

/// Keeps every event as one line of the log, and where its parts are in it. The line is the
/// compact format the terminal shows: time, level, the spans it is in, its target, the message,
/// its fields and then the spans' fields.
struct StoredLayer {
    output: Arc<DiagnosticState>,
}

/// A span's fields as text, kept on the span for the events inside it.
struct SpanFields(String);

impl<S> Layer<S> for StoredLayer
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        context: Context<'_, S>,
    ) {
        let mut visitor = EntryVisitor::default();
        attributes.record(&mut visitor);
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(SpanFields(visitor.fields));
        }
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        context: Context<'_, S>,
    ) {
        let Some(span) = context.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        if let Some(SpanFields(fields)) = extensions.get_mut::<SpanFields>() {
            let mut visitor = EntryVisitor {
                message: String::new(),
                fields: std::mem::take(fields),
            };
            values.record(&mut visitor);
            *fields = visitor.fields;
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
        use std::fmt::Write as _;

        let mut line = String::new();
        let _ = SystemTime.format_time(&mut Writer::new(&mut line));
        let metadata = event.metadata();
        let _ = write!(line, " {:>5} ", metadata.level());
        let origin = line.len();
        let mut span_fields = String::new();
        if let Some(scope) = context.event_scope(event) {
            for span in scope.from_root() {
                line.push_str(span.name());
                line.push(':');
                if let Some(SpanFields(fields)) = span.extensions().get::<SpanFields>()
                    && !fields.is_empty()
                {
                    span_fields.push(' ');
                    span_fields.push_str(fields);
                }
            }
            if line.len() > origin {
                line.push(' ');
            }
        }
        line.push_str(metadata.target());
        let origin_end = line.len();
        line.push_str(": ");
        let mut visitor = EntryVisitor::default();
        event.record(&mut visitor);
        line.push_str(&visitor.message);
        let message_end = line.len();
        if !visitor.fields.is_empty() {
            line.push(' ');
            line.push_str(&visitor.fields);
        }
        line.push_str(&span_fields);
        line.push('\n');
        let level = match *metadata.level() {
            tracing::Level::ERROR => DiagnosticLevel::Error,
            tracing::Level::WARN => DiagnosticLevel::Warning,
            tracing::Level::INFO => DiagnosticLevel::Info,
            tracing::Level::DEBUG => DiagnosticLevel::Debug,
            tracing::Level::TRACE => DiagnosticLevel::Trace,
        };
        let time_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        self.output.append(
            line,
            Some(Parts {
                time_millis,
                level,
                origin,
                origin_end,
                message_end,
            }),
        );
    }
}

#[derive(Clone, Copy)]
struct PrivacyFields;

impl<'writer> FormatFields<'writer> for PrivacyFields {
    fn format_fields<R: RecordFields>(
        &self,
        mut writer: Writer<'writer>,
        fields: R,
    ) -> fmt::Result {
        let mut visitor = EntryVisitor::default();
        fields.record(&mut visitor);
        writer.write_str(&visitor.message)?;
        if !visitor.message.is_empty() && !visitor.fields.is_empty() {
            writer.write_char(' ')?;
        }
        writer.write_str(&visitor.fields)
    }
}

/// An event's message and its other fields as `name=value`, each with secrets and paths taken out.
#[derive(Default)]
struct EntryVisitor {
    message: String,
    fields: String,
}

impl EntryVisitor {
    fn record_value(&mut self, field: &Field, value: String) {
        let name = field.name();
        let value = sanitize_field(name, &value);
        if name == "message" {
            self.message = value;
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            self.fields.push_str(name);
            self.fields.push('=');
            self.fields.push_str(&value);
        }
    }
}

impl Visit for EntryVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_value(field, value.to_string());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.record_value(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.record_value(field, format!("{value:?}"));
    }
}

fn sanitize_field(name: &str, value: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if ["password", "token", "secret", "authorization", "cookie"]
        .iter()
        .any(|part| lower.contains(part))
    {
        return "<redacted>".to_string();
    }
    if lower == "path"
        || lower.ends_with("_path")
        || lower == "root"
        || lower.ends_with("_root")
        || lower == "directory"
        || lower.ends_with("_directory")
    {
        return summarize_path(value);
    }
    if lower == "public_url" {
        return summarize_public_url(value);
    }
    if lower == "url" || lower.ends_with("_url") || lower == "uri" || lower.ends_with("_uri") {
        return summarize_url(value);
    }
    sanitize_free_text(value)
}

fn summarize_path(value: &str) -> String {
    let value = value
        .trim()
        .trim_matches('"')
        .trim_start_matches("Some(\"")
        .trim_end_matches("\")");
    let Some(name) = value
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
    else {
        return "<local>".to_string();
    };
    format!("<local>/{name}")
}

fn summarize_url(value: &str) -> String {
    let value = value.trim().trim_matches('"');
    let Ok(url) = url::Url::parse(value) else {
        return "<url>".to_string();
    };
    if url.scheme() == "file" {
        return summarize_path(url.path());
    }
    let mut summarized = format!("{}://<server>{}", url.scheme(), url.path());
    let keys = url.query_pairs().map(|(key, _)| key).collect::<Vec<_>>();
    if !keys.is_empty() {
        summarized.push_str("?keys=");
        summarized.push_str(&keys.join(","));
    }
    summarized
}

fn summarize_public_url(value: &str) -> String {
    let value = value.trim().trim_matches('"');
    let Ok(url) = url::Url::parse(value) else {
        return "<url>".to_string();
    };
    let origin = url.origin().ascii_serialization();
    if origin == "null" {
        return summarize_url(value);
    }
    let mut summarized = format!("{origin}{}", url.path());
    let keys = url.query_pairs().map(|(key, _)| key).collect::<Vec<_>>();
    if !keys.is_empty() {
        summarized.push_str("?keys=");
        summarized.push_str(&keys.join(","));
    }
    summarized
}

#[expect(
    clippy::string_slice,
    reason = "URL offsets come from ASCII finds and terminators come from char-aware finds"
)]
fn sanitize_free_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some((index, _scheme)) = next_url(rest) {
        output.push_str(&rest[..index]);
        let candidate = &rest[index..];
        let end = candidate
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '"' | '\'' | ')' | ']' | '>' | ',')
            })
            .unwrap_or(candidate.len());
        let raw = &candidate[..end];
        output.push_str(&summarize_url(raw));
        rest = &candidate[end..];
    }
    output.push_str(rest);
    redact_local_paths(&redact_embedded_secrets(&output))
}

fn next_url(value: &str) -> Option<(usize, &'static str)> {
    ["https://", "http://", "file://"]
        .into_iter()
        .filter_map(|scheme| value.find(scheme).map(|index| (index, scheme)))
        .min_by_key(|(index, _)| *index)
}

#[expect(
    clippy::string_slice,
    reason = "secret offsets come from ASCII field names and char-aware separators"
)]
fn redact_embedded_secrets(value: &str) -> String {
    const NAMES: [&str; 7] = [
        "authorization",
        "password",
        "api_key",
        "apikey",
        "cookie",
        "secret",
        "token",
    ];

    let lower = value.to_ascii_lowercase();
    let mut ranges = Vec::new();
    for name in NAMES {
        let mut start = 0;
        while let Some(relative) = lower[start..].find(name) {
            let name_start = start + relative;
            let after_name = name_start + name.len();
            let separator = after_name
                + value[after_name..]
                    .len()
                    .saturating_sub(value[after_name..].trim_start().len());
            let Some(separator_char) = value[separator..].chars().next() else {
                break;
            };
            if matches!(separator_char, '=' | ':') {
                let after_separator = separator + separator_char.len_utf8();
                let value_start = after_separator
                    + value[after_separator..]
                        .len()
                        .saturating_sub(value[after_separator..].trim_start().len());
                let (value_start, value_end) =
                    if matches!(value[value_start..].chars().next(), Some('"' | '\'')) {
                        let quote = value[value_start..]
                            .chars()
                            .next()
                            .expect("quoted secret must have an opening quote");
                        let content_start = value_start + quote.len_utf8();
                        let content_end = value[content_start..]
                            .find(quote)
                            .map_or(value.len(), |end| content_start + end);
                        (content_start, content_end)
                    } else {
                        let value_end = value[value_start..]
                            .find(|character: char| {
                                character.is_whitespace()
                                    || matches!(character, '&' | ',' | ';' | ')' | ']' | '}')
                            })
                            .map_or(value.len(), |end| value_start + end);
                        (value_start, value_end)
                    };
                if value_end > value_start {
                    ranges.push((value_start, value_end));
                }
            }
            start = after_name;
        }
    }
    replace_ranges(value, ranges, "<redacted>")
}

#[expect(
    clippy::string_slice,
    reason = "path offsets come from ASCII prefixes and char-aware terminators"
)]
fn redact_local_paths(value: &str) -> String {
    let mut prefixes = vec![
        "/var/home/".to_string(),
        "/home/".to_string(),
        "/run/user/".to_string(),
        r"C:\Users\".to_string(),
    ];
    for variable in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(variable).and_then(|home| home.into_string().ok())
            && (home.starts_with('/') || home.as_bytes().get(1) == Some(&b':'))
        {
            prefixes.push(home);
        }
    }

    let mut ranges = Vec::new();
    for prefix in prefixes {
        let mut start = 0;
        while let Some(relative) = value[start..].find(&prefix) {
            let path_start = start + relative;
            let quote = value[..path_start]
                .chars()
                .next_back()
                .filter(|character| matches!(character, '"' | '\''));
            let path_end = value[path_start..]
                .find(|character: char| {
                    quote.map_or(
                        character.is_whitespace()
                            || matches!(character, '"' | '\'' | ')' | ']' | '}' | '>' | ','),
                        |quote| character == quote,
                    )
                })
                .map_or(value.len(), |end| path_start + end);
            if path_end > path_start {
                ranges.push((path_start, path_end));
            }
            start = path_start + prefix.len();
        }
    }

    ranges.sort_unstable();
    ranges.dedup();
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    for (start, end) in ranges {
        if start < cursor {
            continue;
        }
        output.push_str(&value[cursor..start]);
        output.push_str(&summarize_path(&value[start..end]));
        cursor = end;
    }
    output.push_str(&value[cursor..]);
    output
}

#[expect(
    clippy::string_slice,
    reason = "replacement ranges are produced from character-boundary searches above"
)]
fn replace_ranges(value: &str, mut ranges: Vec<(usize, usize)>, replacement: &str) -> String {
    ranges.sort_unstable();
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    for (start, end) in ranges {
        if start < cursor {
            continue;
        }
        output.push_str(&value[cursor..start]);
        output.push_str(replacement);
        cursor = end;
    }
    output.push_str(&value[cursor..]);
    output
}

fn install_panic_hook(log_dir: PathBuf, stderr: stderr::Writer) {
    let write_lock = Arc::new(Mutex::new(()));
    std::panic::set_hook(Box::new(move |panic| {
        let _guard = write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let report = panic_report(panic);
        let _ = write_panic_report(&log_dir, &report);
        // The default hook writes synchronously to stderr and can block on the
        // same stalled destination as ordinary logging. The file above is primary.
        stderr.enqueue(report.into_bytes());
    }));
}

fn panic_report(panic: &std::panic::PanicHookInfo<'_>) -> String {
    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("unnamed");
    let location = panic
        .location()
        .map(|location| {
            format!(
                "{}:{}:{}",
                summarize_path(location.file()),
                location.line(),
                location.column()
            )
        })
        .unwrap_or_else(|| "unknown".to_string());
    let message = panic
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    let mut report = sanitize_free_text(&format!(
        "Rufin {}\nthread: {thread_name}\nlocation: {location}\nmessage: {message}\n\n{:?}\n",
        env!("CARGO_PKG_VERSION"),
        Backtrace::force_capture()
    ));
    report.truncate(report.floor_char_boundary(BUFFER_MAX_BYTES));
    report
}

fn write_panic_report(log_dir: &Path, report: &str) -> io::Result<()> {
    use std::io::Write as _;

    fs::create_dir_all(log_dir)?;
    rotate_files(log_dir, PANIC_FILE)?;
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(log_dir.join(PANIC_FILE))?
        .write_all(report.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_logs_keep_snapshot_order_and_drain_through_rotation() {
        let directory = tempfile::tempdir().unwrap();
        let (output, guard) = DiagnosticState::new(directory.path());
        std::thread::scope(|scope| {
            for producer in 0..4 {
                let output = Arc::clone(&output);
                scope.spawn(move || {
                    for index in 0..256 {
                        output.append(format!("{producer}:{index}:{}\n", "x".repeat(4096)), None);
                    }
                });
            }
        });
        drop(guard);
        let snapshot = output.output.lock().unwrap().text(0);
        let mut persisted = String::new();
        for name in ["rufin.log.2", "rufin.log.1", LOG_FILE] {
            if let Ok(contents) = fs::read_to_string(directory.path().join(name)) {
                persisted.push_str(&contents);
            }
        }
        let records = persisted
            .lines()
            .filter(|line| !line.starts_with("WARN "))
            .count();
        assert!(records <= 1024);
        if records < 1024 {
            assert!(persisted.contains(OMITTED_RECORDS));
        }
        assert!(persisted.ends_with(&snapshot));
        assert!(snapshot.len() <= BUFFER_MAX_BYTES);
    }

    #[test]
    fn persistent_log_failure_is_reported_in_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let (output, guard) = DiagnosticState::new(directory.path());
        // A directory at the rotation destination makes rotation fail on all platforms.
        fs::create_dir(directory.path().join("rufin.log.2")).unwrap();
        output.append("x".repeat(BUFFER_MAX_BYTES), None);
        output.flush();
        output.append("rotation triggers here\n".into(), None);
        drop(guard);
        let snapshot = output.output.lock().unwrap().text(0);
        assert!(snapshot.contains("rotation triggers here"));
        assert!(snapshot.contains("local log file is unavailable"));
    }

    #[test]
    fn redirected_diagnostics_drain_to_file_on_shutdown() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let (writer, guard) = stderr::Writer::new(file.reopen().unwrap());
        let subscriber = Registry::default().with(
            tracing_fmt::layer()
                .without_time()
                .with_ansi(false)
                .with_writer(writer),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!("redirected warning");
            tracing::error!("redirected error");
        });
        drop(guard);
        let contents = fs::read_to_string(file.path()).unwrap();
        assert!(contents.contains("redirected warning"));
        assert!(contents.contains("redirected error"));
        assert_eq!(contents.lines().count(), 2);
    }

    #[test]
    fn stalled_stderr_does_not_block_local_logs_or_shutdown() {
        use std::sync::mpsc;
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let (output, persistent) = DiagnosticState::new(directory.path());
        let (reader, pipe) = io::pipe().unwrap();
        let (writer, guard) = stderr::Writer::new(pipe);
        let subscriber = Registry::default()
            .with(tracing_fmt::layer().with_writer(writer))
            .with(StoredLayer {
                output: Arc::clone(&output),
            });
        let (finished, completion) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            tracing::subscriber::with_default(subscriber, || {
                // Exceed the real pipe and stderr queue capacities without a reader.
                let message = "x".repeat(64 * 1024);
                for _ in 0..256 {
                    tracing::warn!("{message}");
                }
                tracing::error!("local log still available");
            });
            drop(persistent);
            drop(guard);
            finished.send(()).unwrap();
        });
        let result = completion.recv_timeout(Duration::from_secs(5));
        // Release the worker even if the assertion fails; don't leak a blocked writer.
        drop(reader);
        producer.join().unwrap();
        result.expect("stderr blocked the logging caller or shutdown");
        assert!(
            output
                .output
                .lock()
                .unwrap()
                .text(0)
                .contains("local log still available")
        );
        assert!(
            fs::read_to_string(directory.path().join(LOG_FILE))
                .unwrap()
                .contains("local log still available")
        );
    }

    #[test]
    fn curated_debug_keeps_public_requests_private_and_suppresses_http2_frames() {
        let output = Arc::new(DiagnosticState {
            output: Mutex::new(DiagnosticOutput::default()),
            ready: Condvar::new(),
            drained: Condvar::new(),
        });
        let subscriber = Registry::default()
            .with(profile_filter(true))
            .with(StoredLayer {
                output: Arc::clone(&output),
            });

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "h2::codec", "transport frame");
            tracing::debug!(target: "ui::shell::shell::route", route = "Home", "navigate");
            tracing::debug!(target: "ui::library::routes", "route rows ready");
            tracing::debug!(target: "sources", endpoint = "/rest/getSongs", "source request");
            tracing::debug!(
                target: "sources",
                url = "https://private.test/Users/abc/Views?api_key=secret",
                "source URL"
            );
            tracing::debug!(
                target: "metadata_lookup",
                public_url = "https://coverartarchive.org/release/abc/front?size=250&token=secret",
                "public request"
            );
        });

        let output = output
            .output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = output.text(0);
        let navigate = output
            .updates(0)
            .entries
            .into_iter()
            .find(|entry| entry.message == "navigate")
            .expect("the navigate entry");
        assert_eq!(navigate.level, Some(DiagnosticLevel::Debug));
        assert_eq!(navigate.origin, "ui::shell::shell::route");
        assert_eq!(navigate.fields, "route=Home");
        assert!(snapshot.contains("source request"));
        assert!(snapshot.contains("navigate route=Home"));
        assert!(snapshot.contains("route rows ready"));
        assert!(snapshot.contains("url=https://<server>/Users/abc/Views?keys=api_key"));
        assert!(
            snapshot.contains(
                "public_url=https://coverartarchive.org/release/abc/front?keys=size,token"
            )
        );
        assert!(!snapshot.contains("transport frame"));
        assert!(!snapshot.contains("private.test"));
        assert!(!snapshot.contains("secret"));
        assert!(!snapshot.contains('\u{1b}'));
    }

    #[test]
    fn privacy_format_keeps_track_ids_and_removes_url_secrets() {
        let text = sanitize_free_text(
            "track_id=navidrome:track:42 https://private.test/rest/stream?id=42&token=secret",
        );

        assert!(text.contains("track_id=navidrome:track:42"));
        assert!(text.contains("https://<server>/rest/stream?keys=id,token"));
        assert!(!text.contains("private.test"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn malformed_url_fragments_stop_at_a_redacted_value() {
        assert_eq!(
            sanitize_free_text("request failed for https://[invalid"),
            "request failed for <url>"
        );
    }

    #[test]
    fn path_fields_keep_only_the_filename() {
        assert_eq!(
            sanitize_field("source_path", "/home/example/Music/Artist/Track.flac"),
            "<local>/Track.flac"
        );
        assert_eq!(
            sanitize_field("root", "/var/home/example/Music/King Crimson"),
            "<local>/King Crimson"
        );
        assert_eq!(
            sanitize_field("endpoint", "/Users/abc/Views"),
            "/Users/abc/Views"
        );
    }

    #[test]
    fn embedded_paths_and_credentials_are_redacted_without_losing_media_facts() {
        let text = sanitize_free_text(
            "failed \"/var/home/screwy/Music/King Crimson/Red.flac\" token = \"private value\" \
             format=flac bitrate_kbps=320 track_id=local:track:7",
        );

        assert!(text.contains("<local>/Red.flac"));
        assert!(text.contains("token = \"<redacted>\""));
        assert!(text.contains("format=flac bitrate_kbps=320"));
        assert!(text.contains("track_id=local:track:7"));
        assert!(!text.contains("screwy"));
        assert!(!text.contains("private"));
    }

    #[test]
    fn diagnostic_buffer_keeps_a_bounded_tail() {
        let output = DiagnosticState {
            output: Mutex::new(DiagnosticOutput::default()),
            ready: Condvar::new(),
            drained: Condvar::new(),
        };
        // The producer completes even before a file writer can run.
        output.append("猫".repeat(BUFFER_MAX_BYTES / 3), None);
        let first = output.output.lock().unwrap().updates(0);
        output.append("tail\n".to_string(), None);
        let update = output.output.lock().unwrap().updates(first.end);
        assert_eq!(update.entries.len(), 1);
        assert_eq!(update.entries[0].message, "tail");
        assert_eq!(update.start, first.end);
        {
            let mut buffer = output.output.lock().unwrap();
            assert!(buffer.bytes.len() <= BUFFER_MAX_BYTES);
            buffer.closed = true;
        }
        output.run(None);
        output.flush();
        let buffer = output.output.lock().unwrap();
        assert!(buffer.text(0).contains(OMITTED_RECORDS));
        assert!(buffer.updates(buffer.end()).entries.is_empty());
    }

    #[test]
    fn previous_crash_is_imported_into_the_next_session_log() {
        let directory = tempfile::tempdir().expect("temporary log directory");
        let panic_path = directory.path().join(PANIC_FILE);
        fs::write(&panic_path, "panic at /home/example/Rufin/src/main.rs:20")
            .expect("pending panic report");
        let mut output = DiagnosticOutput::default();

        import_previous_panic_report(directory.path(), &mut output);

        let snapshot = output.text(0);
        assert!(snapshot.contains("Previous Rufin crash report"));
        assert!(snapshot.contains("<local>/main.rs:20"));
        assert!(!panic_path.exists());
    }
}
