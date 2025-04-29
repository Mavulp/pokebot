// TODO Temporary file until we have a better logging setup for slog

use slog::{Drain, KV};
use std::fmt::{self, Arguments, Write};

pub struct LogBridge<T>(pub T);

impl<T: log::Log> Drain for LogBridge<T> {
    type Ok = ();
    type Err = slog::Error;

    fn log(&self, record: &slog::Record, kvs: &slog::OwnedKVList) -> Result<(), Self::Err> {
        let mut target = record.tag();
        if target.is_empty() {
            target = record.module();
        }

        let lazy = LazyLog::new(record, kvs);

        self.0.log(
            &log::Record::builder()
                .args(format_args!("{}", lazy))
                .level(level_to_log(record.level()))
                .target(target)
                .module_path_static(Some(record.module()))
                .file_static(Some(record.file()))
                .line(Some(record.line()))
                .build(),
        );

        Ok(())
    }

    fn is_enabled(&self, level: slog::Level) -> bool {
        let meta = log::Metadata::builder().level(level_to_log(level)).build();

        self.0.enabled(&meta)
    }
}

fn level_to_log(level: slog::Level) -> log::Level {
    match level {
        slog::Level::Critical | slog::Level::Error => log::Level::Error,
        slog::Level::Warning => log::Level::Warn,
        slog::Level::Info => log::Level::Info,
        slog::Level::Debug => log::Level::Debug,
        slog::Level::Trace => log::Level::Trace,
    }
}

struct LazyLog<'a> {
    record: &'a slog::Record<'a>,
    kvs: &'a slog::OwnedKVList,
}

impl<'a> LazyLog<'a> {
    fn new(record: &'a slog::Record, kvs: &'a slog::OwnedKVList) -> Self {
        LazyLog { record, kvs }
    }
}

impl fmt::Display for LazyLog<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.record.msg())?;

        let mut ser = StringSerializer::new();

        self.kvs
            .serialize(self.record, &mut ser)
            .map_err(|_| fmt::Error)?;
        self.record
            .kv()
            .serialize(self.record, &mut ser)
            .map_err(|_| fmt::Error)?;

        write!(f, "{}", ser.finish())
    }
}

struct StringSerializer {
    inner: String,
}

impl StringSerializer {
    fn new() -> Self {
        StringSerializer {
            inner: String::new(),
        }
    }

    fn finish(self) -> String {
        self.inner
    }
}

impl slog::Serializer for StringSerializer {
    fn emit_arguments(&mut self, key: slog::Key, value: &Arguments) -> slog::Result {
        write!(self.inner, ", {}: {}", key, value)?;
        Ok(())
    }
}
