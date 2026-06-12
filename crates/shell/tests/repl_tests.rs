// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Ported from the REPL path of pyfly's `ClickShellAdapter.run_interactive`
//! (`tests/shell/test_click_adapter.py` REPL behavior). Scripted input is fed
//! through an in-memory `BufRead`, with output captured into a `Vec<u8>`, so
//! the loop is fully deterministic and never blocks.

use std::io::Cursor;

use firefly_shell::{handler, CommandArgs, CommandSpec, StdShell, ValueType};

fn echo_shell() -> StdShell {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "greet",
            handler(|a: CommandArgs| async move {
                Ok(format!("Hello, {}!", a.get_str("name").unwrap_or("world")))
            }),
        )
        .arg("name", ValueType::Str),
    );
    shell
}

#[tokio::test]
async fn repl_dispatches_each_line_and_prints_output() {
    let shell = echo_shell();
    // Two commands, a blank line (ignored), then EOF.
    let input = Cursor::new(b"greet Alice\n\ngreet Bob\n".to_vec());
    let mut output: Vec<u8> = Vec::new();

    shell.run_repl(input, &mut output).await.unwrap();

    let text = String::from_utf8(output).unwrap();
    // Prompts are written; both greetings appear in order.
    assert!(text.contains("Hello, Alice!"));
    assert!(text.contains("Hello, Bob!"));
    let alice = text.find("Alice").unwrap();
    let bob = text.find("Bob").unwrap();
    assert!(alice < bob, "Alice should be greeted before Bob");
    // The prompt is emitted at least once.
    assert!(text.contains("> "));
}

#[tokio::test]
async fn repl_stops_at_eof_immediately() {
    let shell = echo_shell();
    let input = Cursor::new(Vec::<u8>::new());
    let mut output: Vec<u8> = Vec::new();
    // Empty input -> EOF on first read -> loop exits after one prompt.
    shell.run_repl(input, &mut output).await.unwrap();
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text, "> ");
}

#[tokio::test]
async fn repl_ignores_blank_lines() {
    let shell = echo_shell();
    let input = Cursor::new(b"\n  \n".to_vec());
    let mut output: Vec<u8> = Vec::new();
    shell.run_repl(input, &mut output).await.unwrap();
    let text = String::from_utf8(output).unwrap();
    // Three prompts (two blank lines + final EOF prompt), no greetings.
    assert!(!text.contains("Hello"));
    assert_eq!(text.matches("> ").count(), 3);
}
