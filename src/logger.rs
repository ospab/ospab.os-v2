/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
*/
#![no_std]

pub enum LogLevel {
    Info,
    Warning,
    Error,
    Debug,
}

pub struct KernelLogger;

impl KernelLogger {
    pub fn log(level: LogLevel, message: &str) {
        let (prefix, _color) = match level {
            LogLevel::Info => ("INFO", ""),
            LogLevel::Warning => ("WARNING", "\x1b[33m"),
            LogLevel::Error => ("ERROR", "\x1b[31m"),
            LogLevel::Debug => ("DEBUG", ""),
        };
        KernelLogger::write(core::format_args!("[{}] {}\n", prefix, message));
    }

    fn write(args: core::fmt::Arguments) {
        crate::console::_print(args);
    }
}
