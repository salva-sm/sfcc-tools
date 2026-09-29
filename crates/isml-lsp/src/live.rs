//! The value under the cursor while debugging. Zed asks a debug adapter nothing on hover, so
//! sfcc-dap answers on localhost.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use sfcc_core::state::is_within;
use sfcc_core::state::sessions::{self, Session};

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

pub fn markdown(file: &Path, line: &str, column: usize) -> Option<String> {
    let script = file
        .extension()
        .is_some_and(|extension| extension == "js" || extension == "ds");
    if !script {
        return None;
    }
    let expression = expression_at(line, column)?;
    let answer = sessions::all()
        .into_iter()
        .filter(|session| is_within(file, Path::new(&session.cartridges)))
        .find_map(|session| ask(&session, &expression))?;
    render(&expression, &answer)
}

/// On `totalGrossPrice` in `order.totalGrossPrice.value`: `order.totalGrossPrice`.
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
