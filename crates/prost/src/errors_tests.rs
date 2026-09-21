use super::{fingerprint, group};
use crate::tail::Entry;

fn entry(moment: &str, lines: &[&str]) -> Entry {
    Entry {
        label: "error".to_string(),
        moment: moment.to_string(),
        lines: lines.iter().map(|line| line.to_string()).collect(),
    }
}

const FAILURE: [&str; 2] = [
    "[2026-09-10 09:00:00.000 GMT] ERROR Sites-guess_fr TypeError: x is undefined",
    " at app_common_eu_guess/cartridge/scripts/x.js:12",
];

#[test]
fn the_same_failure_at_another_moment_has_one_fingerprint() {
    let first = entry("2026-09-10 09:00:00.000 GMT", &FAILURE);
    let second = entry(
        "2026-09-10 09:04:31.000 GMT",
        &[
            "[2026-09-10 09:04:31.000 GMT] ERROR Sites-guess_fr TypeError: x is undefined",
            " at app_common_eu_guess/cartridge/scripts/x.js:12",
        ],
    );
    assert_eq!(fingerprint(&first), fingerprint(&second));
}

#[test]
fn a_different_line_number_is_a_different_failure() {
    let first = entry("2026-09-10 09:00:00.000 GMT", &FAILURE);
    let second = entry(
        "2026-09-10 09:00:00.000 GMT",
        &[FAILURE[0], " at app_common_eu_guess/cartridge/scripts/x.js:99"],
    );
    assert_ne!(fingerprint(&first), fingerprint(&second));
}

#[test]
fn repeats_collapse_and_keep_every_moment_in_order() {
    let entries = vec![
        entry("2026-09-10 09:00:00.000 GMT", &FAILURE),
        entry("2026-09-10 09:01:00.000 GMT", &["[...] ERROR something else"]),
        entry("2026-09-10 09:02:00.000 GMT", &FAILURE),
    ];

    let groups = group(entries);

    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].times.len(), 2);
    assert_eq!(groups[0].times[0], "2026-09-10 09:00:00.000 GMT");
    assert_eq!(groups[0].times[1], "2026-09-10 09:02:00.000 GMT");
    assert_eq!(groups[1].times.len(), 1);
}
