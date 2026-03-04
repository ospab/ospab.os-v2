/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Distributed under the Boost Software License, Version 1.1.
See LICENSE or https://www.boost.org/LICENSE_1_0.txt for details.
*/
#![no_std]

pub struct Command<'a> {
    pub name: &'a str,
    pub args: [&'a str; 5],
}

pub fn parse(input: &str) -> Command {
    let mut parts = input.split_whitespace();
    let name = parts.next().unwrap_or("");
    let mut args = [""; 5];
    for (i, arg) in parts.take(5).enumerate() {
        args[i] = arg;
    }
    Command { name, args }
}
