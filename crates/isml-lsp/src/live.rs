//! The value under the cursor while a debug session is halted, at the top of the hover.
//!
//! The one place this server reaches past the checkout: Zed sends a debug adapter nothing on
//! hover, so sfcc-dap, while attached, answers on a localhost port instead.
//!
//! # The file it reads
//!
//! One JSON file per running adapter under sfcc-dap's own folder, naming its cartridges, port
//! and token - the one layout the two programs have to agree on. One connection carries one
//! JSON line each way.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

/// Past this the hover would rather show only the docs than keep the editor waiting.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(4);
const VALUE: usize = 80;

const KEYWORDS: [&str; 24] = [
    "var",
    "let",
    "const",
    "function",
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "new",
    "typeof",
    "instanceof",
    "in",
    "of",
    "true",
    "false",
    "null",
    "undefined",
    "switch",
    "case",
    "break",
    "continue",
    "throw",
];

#[derive(Debug, serde::Deserialize)]
struct Session {
    cartridges: String,
    port: u16,
    token: String,
}

pub fn markdown(file: &Path, line: &str, column: usize) -> Option<String> {
    let script = file
        .extension()
        .is_some_and(|extension| extension == "js" || extension == "ds");
    if !script {
        return None;
    }
    let expression = expression_at(line, column)?;
    let answer = sessions()
        .into_iter()
        .filter(|session| covers(&session.cartridges, file))
        .find_map(|session| ask(&session, &expression))?;
    render(&expression, &answer)
}

/// The dotted name up to the identifier under the cursor: on `totalGrossPrice` in
/// `order.totalGrossPrice.value`, that is `order.totalGrossPrice`.
pub fn expression_at(line: &str, column: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let part = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    if !chars.get(column).is_some_and(|&c| part(c)) {
        return None;
    }
    let mut end = column;
    while chars.get(end).is_some_and(|&c| part(c)) {
        end += 1;
    }
    let mut start = column;
    loop {
        while start > 0 && part(chars[start - 1]) {
            start -= 1;
        }
        if start >= 2 && chars[start - 1] == '.' && part(chars[start - 2]) {
            start -= 1;
            continue;
        }
        // Past a call or an index the chain is no longer a name the debugger can open.
        if start >= 1 && chars[start - 1] == '.' {
            return None;
        }
        break;
    }
    let expression: String = chars[start..end].iter().collect();
    let first = expression.split('.').next()?;
    let keyword = KEYWORDS.contains(&first) || expression.split('.').any(|part| part.is_empty());
    let number = first.starts_with(|c: char| c.is_ascii_digit());
    (!keyword && !number).then_some(expression)
}

fn sessions() -> Vec<Session> {
    let Some(entries) = sessions_dir().and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| serde_json::from_str(&std::fs::read_to_string(entry.path()).ok()?).ok())
        .collect()
}

/// Case and separators aside: `dw.json` and the editor rarely spell a Windows path alike.
fn covers(cartridges: &str, file: &Path) -> bool {
    let normal = |path: &str| path.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let root = normal(cartridges);
    let file = normal(&file.to_string_lossy());
    !root.is_empty() && file.starts_with(&format!("{root}/"))
}

fn ask(session: &Session, expression: &str) -> Option<Value> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, session.port));
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(ANSWER_TIMEOUT)).ok()?;
    let asked = serde_json::json!({ "token": session.token, "expression": expression });
    writeln!(stream, "{asked}").ok()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// Nothing when nothing is halted, or the name means nothing in the frame.
pub fn render(expression: &str, answer: &Value) -> Option<String> {
    let value = answer.get("value")?.as_str()?;
    let mut out = format!("**{expression}**");
    if let Some(kind) = answer.get("type").and_then(Value::as_str) {
        out.push_str(&format!(" · `{kind}`"));
    }
    out.push_str(&format!("\n```js\n{value}\n```\n"));

    let members = answer
        .get("members")
        .and_then(Value::as_array)
        .filter(|members| !members.is_empty());
    if let Some(members) = members {
        out.push_str("\n| | | |\n| :-- | :-- | :-- |\n");
        for member in members {
            let text = |key: &str| member.get(key).and_then(Value::as_str).unwrap_or_default();
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                cell(text("name")),
                cell(text("type")),
                cell(&shorten(text("value"))),
            ));
        }
    }
    Some(out)
}

fn shorten(value: &str) -> String {
    if value.chars().count() <= VALUE {
        return value.to_string();
    }
    let cut: String = value.chars().take(VALUE - 1).collect();
    format!("{cut}…")
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// sfcc-dap's folder, as it derives it.
fn sessions_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return Some(PathBuf::from(appdata).join("sfcc-dap").join("sessions"));
        }
    }
    let root = match std::env::var("XDG_CONFIG_HOME") {
        Ok(config) if !config.is_empty() => PathBuf::from(config),
        _ => PathBuf::from(std::env::var("HOME").ok()?).join(".config"),
    };
    Some(root.join("sfcc-dap").join("sessions"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: &str, word: &str) -> Option<String> {
        expression_at(line, line.find(word).unwrap())
    }

    #[test]
    fn reads_the_chain_up_to_the_word_under_the_cursor() {
        let line = "    var points = order.totalGrossPrice.value * rate;";
        assert_eq!(at(line, "order").as_deref(), Some("order"));
        assert_eq!(at(line, "total").as_deref(), Some("order.totalGrossPrice"));
        assert_eq!(
            at(line, "value").as_deref(),
            Some("order.totalGrossPrice.value")
        );
        assert_eq!(at(line, "rate").as_deref(), Some("rate"));
    }

    #[test]
    fn leaves_calls_keywords_and_blanks_alone() {
        assert_eq!(at("basket.getTotal().value", "value"), None);
        assert_eq!(at("items[0].price", "price"), None);
        assert_eq!(at("    var points = 1;", "var"), None);
        assert_eq!(expression_at("a  b", 1), None);
    }

    #[test]
    fn matches_a_checkout_however_the_path_is_spelled() {
        let file = Path::new(r"C:\Dev\shop\cartridges\app\cartridge\scripts\a.js");
        assert!(covers("c:/dev/shop/cartridges/", file));
        assert!(!covers(r"C:\dev\shop\cartridges-old", file));
    }

    #[test]
    fn shows_the_value_and_one_level_of_members() {
        let answer = serde_json::json!({
            "value": "EUR 125.00",
            "type": "dw.value.Money",
            "members": [
                { "name": "value", "type": "Number", "value": "125" },
                { "name": "currencyCode", "type": "String", "value": "a|b" },
            ],
        });
        let shown = render("order.totalGrossPrice", &answer).unwrap();
        assert!(shown.starts_with("**order.totalGrossPrice** · `dw.value.Money`"));
        assert!(shown.contains("EUR 125.00"));
        assert!(shown.contains("| value | Number | 125 |"));
        assert!(shown.contains("a\\|b"));
    }

    #[test]
    fn shows_nothing_when_nothing_is_halted() {
        assert_eq!(render("order", &serde_json::json!({})), None);
        assert_eq!(
            render("order", &serde_json::json!({ "error": "ReferenceError" })),
            None
        );
    }
}
