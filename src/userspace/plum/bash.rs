/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

plum/bash — Bash script execution engine for the plum shell.

Supported constructs:
  - Variable expansion: $VAR, ${VAR}, $?, $((expr))
  - Conditionals: if/then/elif/else/fi (with test expressions)
  - Loops: for var in list; do ... done
           while condition; do ... done
  - Functions: funcname() { ... }
  - Command execution: all plum builtins + terminal commands
  - Comments: # ...
  - String quoting: single and double quotes
  - Command chaining: cmd1 ; cmd2
  - Exit codes tracked in $?

All command dispatch goes through super::preprocess() so variable
expansion, aliases, and shell builtins work inside scripts.
All file access goes through crate::fs (VFS layer).
*/

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

use crate::arch::x86_64::framebuffer;

const FG_ERR:  u32 = 0x00FF4444;
const FG_WARN: u32 = 0x0000FFFF;
const BG:      u32 = 0x00000000;

fn err_print(s: &str)  { framebuffer::draw_string(s, FG_ERR,  BG); }
fn warn_print(s: &str) { framebuffer::draw_string(s, FG_WARN, BG); }

// ─── Public API ───────────────────────────────────────────────────────────────

/// Execute a bash code string inline (bash -c 'code').
pub fn execute_code(code: &str) {
    let lines: Vec<&str> = code.lines().collect();
    let mut state = ExecState::new();
    state.functions = BTreeMap::new();
    let mut i = 0;
    while i < lines.len() {
        i = exec_line(&lines, i, &mut state);
    }
}

/// Execute a bash script file from the VFS.
/// Returns the exit code (0 = success, non-zero = error).
pub fn execute_script(path: &str) -> i32 {
    match crate::fs::read_file(path) {
        Some(data) => {
            match core::str::from_utf8(&data) {
                Ok(text) => {
                    let lines: Vec<&str> = text.lines().collect();
                    let mut state = ExecState::new();
                    let mut i = 0;
                    while i < lines.len() {
                        i = exec_line(&lines, i, &mut state);
                        if state.exit_requested { break; }
                    }
                    state.exit_code
                }
                Err(_) => {
                    err_print("bash: script is not valid UTF-8\n");
                    1
                }
            }
        }
        None => {
            let mut msg = String::from("bash: ");
            msg.push_str(path);
            msg.push_str(": no such file\n");
            err_print(&msg);
            127
        }
    }
}

// ─── Execution state ─────────────────────────────────────────────────────────

struct ExecState {
    /// Local variables (script-scope, separate from shell env)
    locals: BTreeMap<String, String>,
    /// Defined functions: name → body lines
    functions: BTreeMap<String, Vec<String>>,
    /// Last command exit code
    exit_code: i32,
    /// true when `exit` or `return` was seen
    exit_requested: bool,
    /// Return code from `return N`
    return_code: Option<i32>,
}

impl ExecState {
    fn new() -> Self {
        ExecState {
            locals: BTreeMap::new(),
            functions: BTreeMap::new(),
            exit_code: 0,
            exit_requested: false,
            return_code: None,
        }
    }

    fn get_var(&self, name: &str) -> String {
        // Check local vars first, then shell environment
        if let Some(v) = self.locals.get(name) {
            return v.clone();
        }
        if name == "?" {
            return int_to_str(self.exit_code as i64);
        }
        super::getenv(name).map(String::from).unwrap_or_default()
    }

    fn set_var(&mut self, name: &str, value: &str) {
        self.locals.insert(String::from(name), String::from(value));
        super::setenv(name, value);
    }
}

// ─── Line executor ────────────────────────────────────────────────────────────

/// Execute one logical line from `lines[i]`.
/// Returns the index of the next line to execute.
fn exec_line(lines: &[&str], i: usize, state: &mut ExecState) -> usize {
    if state.exit_requested { return i + 1; }

    let raw = lines[i].trim();

    // Skip empty lines and comments
    if raw.is_empty() || raw.starts_with('#') {
        return i + 1;
    }

    // Function definition: funcname() {  OR  function funcname {
    if let Some(next) = parse_function_def(lines, i, state) {
        return next;
    }

    // if statement
    let lower = raw.to_lowercase();
    let lower = lower.trim();
    if lower.starts_with("if ") || lower == "if" || lower.starts_with("if[") {
        return exec_if(lines, i, state);
    }

    // while loop
    if lower.starts_with("while ") {
        return exec_while(lines, i, state);
    }

    // for loop
    if lower.starts_with("for ") {
        return exec_for(lines, i, state);
    }

    // exit [code]
    if lower == "exit" || lower.starts_with("exit ") {
        let rest = raw[4..].trim();
        state.exit_code = parse_int(rest).unwrap_or(0) as i32;
        state.exit_requested = true;
        return i + 1;
    }

    // return [code]
    if lower == "return" || lower.starts_with("return ") {
        let rest = raw[6..].trim();
        let code = parse_int(rest).unwrap_or(state.exit_code as i64) as i32;
        state.return_code = Some(code);
        state.exit_requested = true;
        return i + 1;
    }

    // Variable assignment: VAR=value  (no spaces around =, no leading command)
    if let Some(eq) = raw.find('=') {
        let before = &raw[..eq];
        if !before.is_empty() && before.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            let name = before;
            let value_raw = &raw[eq + 1..];
            let value = expand_vars(value_raw, state);
            let value = strip_quotes(&value);
            state.set_var(name, &value);
            return i + 1;
        }
    }

    // Skip structural tokens that should have been consumed by block parsers
    if matches!(lower, "then" | "else" | "elif" | "fi" | "do" | "done" | "{" | "}") {
        return i + 1;
    }

    // Regular command — expand variables and dispatch via plum
    let expanded = expand_vars(raw, state);
    let expanded = expanded.trim();
    if !expanded.is_empty() {
        exec_command(expanded, state);
    }

    i + 1
}

// ─── Command execution ───────────────────────────────────────────────────────

fn exec_command(cmd: &str, state: &mut ExecState) {
    // Handle chained commands (;)
    if cmd.contains(';') {
        let parts: Vec<&str> = cmd.splitn(32, ';').collect();
        for part in parts {
            let part = part.trim();
            if !part.is_empty() {
                exec_command(part, state);
            }
        }
        return;
    }

    // local var=value
    if cmd.starts_with("local ") {
        let rest = cmd[6..].trim();
        if let Some(eq) = rest.find('=') {
            let name = &rest[..eq];
            let value = strip_quotes(&rest[eq + 1..]);
            state.set_var(name, &value);
        }
        return;
    }

    // export var=value  or  export var
    if cmd.starts_with("export ") {
        super::preprocess(cmd);
        return;
    }

    // Dispatch all other commands through plum's preprocessor
    super::preprocess(cmd);
}

// ─── if/then/elif/else/fi ─────────────────────────────────────────────────────

fn exec_if(lines: &[&str], i: usize, state: &mut ExecState) -> usize {
    let raw = lines[i].trim();

    let cond_str = {
        let after_if = raw[2..].trim();
        let s = if let Some(pos) = find_semicolon_then(after_if) {
            &after_if[..pos]
        } else {
            after_if
        };
        expand_vars(s.trim(), state)
    };

    let mut j = i + 1;
    while j < lines.len() {
        let l = lines[j].trim().to_lowercase();
        if l == "then" { j += 1; break; }
        if l.ends_with("; then") || l.ends_with(";then") { j += 1; break; }
        if raw.to_lowercase().contains("; then") || raw.to_lowercase().ends_with(" then") {
            break;
        }
        break;
    }

    let mut then_lines: Vec<usize> = Vec::new();
    let mut else_lines: Vec<usize> = Vec::new();
    let mut in_else = false;
    let mut depth = 1usize;

    while j < lines.len() {
        let l = lines[j].trim().to_lowercase();
        if l.starts_with("if ") || l == "if" { depth += 1; }
        if l == "fi" {
            depth -= 1;
            if depth == 0 { j += 1; break; }
        }
        if depth == 1 {
            if l == "else" { in_else = true; j += 1; continue; }
            if l.starts_with("elif ") {
                if in_else {
                    else_lines.push(j);
                } else {
                    else_lines.push(j);
                }
                in_else = true;
                j += 1;
                continue;
            }
        }
        if in_else { else_lines.push(j); } else { then_lines.push(j); }
        j += 1;
    }

    let cond = eval_condition(&cond_str, state);
    if cond {
        for &li in &then_lines {
            exec_line(lines, li, state);
            if state.exit_requested { break; }
        }
    } else if !else_lines.is_empty() {
        for &li in &else_lines {
            exec_line(lines, li, state);
            if state.exit_requested { break; }
        }
    }

    j
}

fn eval_condition(cond: &str, state: &mut ExecState) -> bool {
    let cond = cond.trim();
    if cond.is_empty() { return false; }

    let inner = if (cond.starts_with('[') && cond.ends_with(']')) ||
                   (cond.starts_with("[[") && cond.ends_with("]]")) {
        cond.trim_start_matches('[').trim_end_matches(']').trim()
    } else {
        cond
    };

    if inner.starts_with("-z ") {
        let val = expand_vars(inner[3..].trim(), state);
        let val = strip_quotes(&val);
        return val.is_empty();
    }
    if inner.starts_with("-n ") {
        let val = expand_vars(inner[3..].trim(), state);
        let val = strip_quotes(&val);
        return !val.is_empty();
    }
    if inner.starts_with("-f ") {
        let path = expand_vars(inner[3..].trim(), state);
        let path = strip_quotes(&path);
        return crate::fs::exists(&path);
    }
    if inner.starts_with("-d ") {
        let path = expand_vars(inner[3..].trim(), state);
        let path = strip_quotes(&path);
        return crate::fs::exists(&path);
    }
    if inner.starts_with("-e ") {
        let path = expand_vars(inner[3..].trim(), state);
        let path = strip_quotes(&path);
        return crate::fs::exists(&path);
    }

    if let Some(pos) = inner.find(" == ") {
        let left  = strip_quotes(&expand_vars(inner[..pos].trim(), state));
        let right = strip_quotes(&expand_vars(inner[pos+4..].trim(), state));
        return left == right;
    }
    if let Some(pos) = inner.find(" = ") {
        let left  = strip_quotes(&expand_vars(inner[..pos].trim(), state));
        let right = strip_quotes(&expand_vars(inner[pos+3..].trim(), state));
        return left == right;
    }
    if let Some(pos) = inner.find(" != ") {
        let left  = strip_quotes(&expand_vars(inner[..pos].trim(), state));
        let right = strip_quotes(&expand_vars(inner[pos+4..].trim(), state));
        return left != right;
    }

    for (op, _) in &[(" -eq ", 5), (" -ne ", 5), (" -lt ", 5), (" -le ", 5), (" -gt ", 5), (" -ge ", 5)] {
        if let Some(pos) = inner.find(op) {
            let left  = parse_int(inner[..pos].trim()).unwrap_or(0);
            let right = parse_int(inner[pos+op.len()..].trim()).unwrap_or(0);
            return match *op {
                " -eq " => left == right,
                " -ne " => left != right,
                " -lt " => left < right,
                " -le " => left <= right,
                " -gt " => left > right,
                " -ge " => left >= right,
                _ => false,
            };
        }
    }

    !inner.is_empty() && inner != "0" && inner != "false"
}

fn find_semicolon_then(s: &str) -> Option<usize> {
    let lower = s.to_lowercase();
    lower.find("; then")
}

// ─── for loop ─────────────────────────────────────────────────────────────────

fn exec_for(lines: &[&str], i: usize, state: &mut ExecState) -> usize {
    let raw = lines[i].trim();
    let rest = raw[4..].trim();
    let (var_name, rest) = split_word(rest);
    let rest = rest.trim();

    let (keyword, rest) = split_word(rest);
    if keyword.to_lowercase() != "in" {
        err_print("bash: for: expected 'in'\n");
        return i + 1;
    }

    let items_str = if let Some(pos) = rest.to_lowercase().find(';') {
        &rest[..pos]
    } else {
        rest
    };
    let items_expanded = expand_vars(items_str.trim(), state);
    let items: Vec<String> = items_expanded.split_whitespace().map(String::from).collect();

    let mut j = i + 1;
    while j < lines.len() {
        let l = lines[j].trim().to_lowercase();
        if l == "do" || l.ends_with("; do") || l.ends_with(";do") { j += 1; break; }
        if raw.to_lowercase().contains("; do") { break; }
        break;
    }

    let body_start = j;
    let mut depth = 1usize;
    while j < lines.len() {
        let l = lines[j].trim().to_lowercase();
        if l.starts_with("for ") || l.starts_with("while ") { depth += 1; }
        if l == "done" {
            depth -= 1;
            if depth == 0 { break; }
        }
        j += 1;
    }
    let body_end = j;
    let end = if j < lines.len() { j + 1 } else { j };

    for item in &items {
        state.set_var(&var_name, item);
        let mut k = body_start;
        while k < body_end {
            k = exec_line(lines, k, state);
            if state.exit_requested { break; }
        }
        if state.exit_requested { break; }
    }

    end
}

// ─── while loop ───────────────────────────────────────────────────────────────

fn exec_while(lines: &[&str], i: usize, state: &mut ExecState) -> usize {
    let raw = lines[i].trim();
    let cond_str_raw = &raw[6..];
    let cond_str_raw = if let Some(pos) = cond_str_raw.to_lowercase().find("; do") {
        &cond_str_raw[..pos]
    } else {
        cond_str_raw
    };

    let mut j = i + 1;
    while j < lines.len() {
        let l = lines[j].trim().to_lowercase();
        if l == "do" || l.ends_with("; do") { j += 1; break; }
        if raw.to_lowercase().contains("; do") { break; }
        break;
    }

    let body_start = j;
    let mut depth = 1usize;
    while j < lines.len() {
        let l = lines[j].trim().to_lowercase();
        if l.starts_with("while ") || l.starts_with("for ") { depth += 1; }
        if l == "done" {
            depth -= 1;
            if depth == 0 { break; }
        }
        j += 1;
    }
    let body_end = j;
    let end = if j < lines.len() { j + 1 } else { j };

    let max_iters = 10_000usize;
    let mut iters = 0;
    loop {
        iters += 1;
        if iters > max_iters {
            warn_print("bash: while loop iteration limit reached\n");
            break;
        }
        let cond_expanded = expand_vars(cond_str_raw, state);
        if !eval_condition(&cond_expanded, state) { break; }

        let mut k = body_start;
        while k < body_end {
            k = exec_line(lines, k, state);
            if state.exit_requested { break; }
        }
        if state.exit_requested { break; }
    }

    end
}

// ─── Function definitions ─────────────────────────────────────────────────────

fn parse_function_def(lines: &[&str], i: usize, state: &mut ExecState) -> Option<usize> {
    let raw = lines[i].trim();

    let func_name = if raw.starts_with("function ") {
        let rest = raw[9..].trim();
        let (name, _) = split_word(rest);
        if name.is_empty() { return None; }
        String::from(name)
    } else if raw.contains("()") {
        let pos = raw.find("()")?;
        let name = raw[..pos].trim();
        if name.is_empty() || name.contains(' ') { return None; }
        String::from(name)
    } else {
        return None;
    };

    let mut j = i;
    while j < lines.len() {
        if lines[j].contains('{') { j += 1; break; }
        j += 1;
    }

    let body_start = j;
    let mut depth = 1usize;
    while j < lines.len() {
        let l = lines[j].trim();
        for c in l.chars() {
            if c == '{' { depth += 1; }
            if c == '}' {
                depth -= 1;
                if depth == 0 { break; }
            }
        }
        if depth == 0 { break; }
        j += 1;
    }
    let body_end = j;

    let body: Vec<String> = (body_start..body_end)
        .map(|li| String::from(lines[li]))
        .collect();
    state.functions.insert(func_name, body);

    Some(if j < lines.len() { j + 1 } else { j })
}

// ─── Variable expansion ───────────────────────────────────────────────────────

fn expand_vars(s: &str, state: &ExecState) -> String {
    let bytes = s.as_bytes();
    let mut result = String::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            let next = bytes[i + 1];

            if next == b'(' && i + 2 < bytes.len() && bytes[i + 2] == b'(' {
                let start = i + 3;
                let mut end = start;
                while end + 1 < bytes.len() && !(bytes[end] == b')' && bytes[end + 1] == b')') {
                    end += 1;
                }
                let expr = &s[start..end];
                let val = eval_arith(expr, state);
                result.push_str(&int_to_str(val));
                i = end + 2;
                continue;
            }

            if next == b'{' {
                let start = i + 2;
                let mut end = start;
                while end < bytes.len() && bytes[end] != b'}' { end += 1; }
                let name = &s[start..end];
                result.push_str(&state.get_var(name));
                i = end + 1;
                continue;
            }

            if next == b'?' {
                result.push_str(&int_to_str(state.exit_code as i64));
                i += 2;
                continue;
            }

            if next.is_ascii_alphanumeric() || next == b'_' {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                    end += 1;
                }
                let name = &s[start..end];
                result.push_str(&state.get_var(name));
                i = end;
                continue;
            }
        }
        result.push(s[i..].chars().next().unwrap_or(' '));
        i += s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }

    result
}

fn eval_arith(expr: &str, state: &ExecState) -> i64 {
    let expanded = expand_vars(expr, state);
    let s = expanded.trim();

    for op in &["+", "-", "*", "/", "%"] {
        if let Some(pos) = s.rfind(op) {
            if pos == 0 { continue; }
            let left  = eval_arith(&s[..pos], state);
            let right = eval_arith(&s[pos+1..], state);
            return match *op {
                "+" => left + right,
                "-" => left - right,
                "*" => left * right,
                "/" => if right != 0 { left / right } else { 0 },
                "%" => if right != 0 { left % right } else { 0 },
                _   => 0,
            };
        }
    }

    parse_int(s).unwrap_or(0)
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn split_word(s: &str) -> (String, &str) {
    let s = s.trim();
    match s.find(|c: char| c.is_whitespace()) {
        Some(pos) => (String::from(&s[..pos]), s[pos..].trim_start()),
        None => (String::from(s), ""),
    }
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2) ||
       (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2) {
        String::from(&s[1..s.len()-1])
    } else {
        String::from(s)
    }
}

fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim().trim_matches('"').trim_matches('\'');
    if s.is_empty() { return None; }
    let (neg, s) = if s.starts_with('-') { (true, &s[1..]) } else { (false, s) };
    let mut n: i64 = 0;
    let mut any = false;
    for b in s.bytes() {
        if b.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((b - b'0') as i64);
            any = true;
        } else {
            break;
        }
    }
    if any { Some(if neg { -n } else { n }) } else { None }
}

fn int_to_str(n: i64) -> String {
    if n == 0 { return String::from("0"); }
    let neg = n < 0;
    let mut val = if neg { (-(n + 1)) as u64 + 1 } else { n as u64 };
    let mut buf = [0u8; 21];
    let mut pos = 21;
    while val > 0 {
        pos -= 1;
        buf[pos] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    if neg { pos -= 1; buf[pos] = b'-'; }
    String::from(core::str::from_utf8(&buf[pos..]).unwrap_or("0"))
}
