/// ASCII logo (VGA-safe, no ANSI). Manifest p.20: first screen.
pub fn print_logo() {
    ospab_os::arch::x86_64::serial::write_str("   ___   _________________  _  __________ \r\n");
    ospab_os::arch::x86_64::serial::write_str("  / _ | / __/_  __/ __/ _ \\/ |/ / __/ _ \\ \r\n");
    ospab_os::arch::x86_64::serial::write_str(" / __ |/ _/  / / / _// , _/    / _// __/ \r\n");
    ospab_os::arch::x86_64::serial::write_str("/_/ |_/_/___/ /_/ /___/_/|_/_/|_/___/_/    \r\n");
    ospab_os::arch::x86_64::serial::write_str("\r\n");
    ospab_os::arch::x86_64::serial::write_str("\r\n");
    ospab_os::arch::x86_64::serial::write_str("                  ospab.os / AETERNA\r\n");
    ospab_os::arch::x86_64::serial::write_str("\r\n");
}

