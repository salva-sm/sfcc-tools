/// `dw` is left out on purpose: it is the whole API namespace.
pub const SFCC_GLOBALS: [&str; 6] = ["pdict", "request", "session", "customer", "response", "out"];

const ENGINE_MEMBERS: [&str; 12] = [
    "constructor",
    "prototype",
    "caller",
    "callee",
    "arguments",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "toLocaleString",
    "valueOf",
    "class",
];

const SUMMARY: usize = 240;

/// Methods are hidden only inside an object; a local holding a function stays.
pub fn worth_showing(name: &str, kind: Option<&str>, inside_object: bool) -> bool {
    if name.starts_with("__") || ENGINE_MEMBERS.contains(&name) {
        return false;
    }
    !(inside_object && kind.is_some_and(|kind| kind.eq_ignore_ascii_case("function")))
}

pub fn looks_like_an_object(value: &str, kind: Option<&str>) -> bool {
    if kind.is_some_and(|kind| kind.eq_ignore_ascii_case("function")) {
        return false;
    }
    value.starts_with("[object ") || kind.is_some_and(|kind| kind.contains('.'))
}

pub fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SUMMARY {
        return flat;
    }
    let cut: String = flat.chars().take(SUMMARY - 1).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_what_the_engine_puts_on_everything() {
        assert!(!worth_showing("hasOwnProperty", Some("function"), false));
        assert!(!worth_showing("__proto__", Some("object"), false));
        assert!(!worth_showing("class", Some("object"), false));
    }

    #[test]
    fn keeps_a_local_that_happens_to_hold_a_function() {
        assert!(worth_showing("callback", Some("function"), false));
        // Inside a dw object, sixty methods are noise.
        assert!(!worth_showing("getCurrency", Some("function"), true));
    }

    #[test]
    fn offers_an_arrow_only_where_there_is_something_behind_it() {
        assert!(looks_like_an_object("[object Object]", Some("Object")));
        assert!(looks_like_an_object("EUR", Some("dw.util.Currency")));
        assert!(!looks_like_an_object("function () {}", Some("function")));
        assert!(!looks_like_an_object("anonymous", Some("string")));
    }

    #[test]
    fn folds_a_value_onto_one_line() {
        assert_eq!(one_line("a\n  b\tc"), "a b c");
        let long = "x".repeat(400);
        let folded = one_line(&long);
        assert_eq!(folded.chars().count(), SUMMARY);
        assert!(folded.ends_with('…'));
    }
}
