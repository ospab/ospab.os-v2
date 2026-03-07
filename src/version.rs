/*
Business Source License 1.1
Copyright (c) 2026 ospab
Central version constants for ospab.os / AETERNA kernel.
After each build iteration, increment PATCH (0-99), then MINOR, etc.
*/

pub const MAJOR: u16 = 1;
pub const MINOR: u16 = 1;
pub const PATCH: u16 = 0;

/// "1.1.0"
pub const VERSION_STR: &str = "1.1.0";

/// "ospab.os v1.1.0"
pub const OS_VERSION: &str = "ospab.os v1.1.0";

/// "AETERNA 1.1.0"
pub const KERNEL_VERSION: &str = "AETERNA 1.1.0";

/// Full uname-style string
pub const UNAME_FULL: &str = "AETERNA 1.1.0 ospab.os x86_64 AETERNA/Microkernel";

/// Build date
pub const BUILD_DATE: &str = "2026-03-07";

/// Architecture
pub const ARCH: &str = "x86_64";
