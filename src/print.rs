/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Simple print macros for kernel debugging
*/

/// Simple println macro that writes to serial
#[macro_export]
macro_rules! println {
    ($($arg:expr) => {
        $crate::arch::x86_64::serial::write_str(concat!($arg, "\r\n"));
    };
}

/// Simple print macro without newline
#[macro_export]
macro_rules! print {
    ($($arg:expr) => {
        $crate::arch::x86_64::serial::write_str($($arg));
    };
}
