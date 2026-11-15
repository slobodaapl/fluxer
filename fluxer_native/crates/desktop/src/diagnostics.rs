// SPDX-License-Identifier: AGPL-3.0-or-later
//! Retain renderer failures in launch stderr even without an external log sink.
struct RendererLog;

impl log::Log for RendererLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
            && (metadata.target().starts_with("blade_graphics")
                || metadata.target().starts_with("gpui::platform"))
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    if log::set_logger(&RendererLog).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}
