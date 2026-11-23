use super::*;

fn roles(line: &str) -> Vec<(Role, String)> {
    segments(line)
        .into_iter()
        .map(|segment| (segment.role, segment.text))
        .collect()
}

fn rejoined(line: &str) -> String {
    segments(line).into_iter().map(|s| s.text).collect()
}

#[test]
fn every_line_rejoins_to_itself() {
    for line in [
        "[18:35:40] [uploaded] int_loyalty/cartridge/scripts/loyalty/constants.js",
        "[17:54:59] [deleted]  int_rewards/cartridge/scripts/vouchers.js",
        "[18:10:45] [error]    cannot read file.js: permission denied",
        "[18:02:10] [uploaded] 7 file(s) in 2 cartridge(s)",
        "[18:34:49] [ok]       version1 already up to date",
        "[18:34:48] [warn]     sandbox unavailable - waiting",
        "[18:00:00]            push handed over",
        "             cartridge/scripts  vouchers.js",
    ] {
        assert_eq!(rejoined(line), line, "{line}");
    }
}

#[test]
fn a_single_upload_splits_into_badge_cartridge_folder_file() {
    assert_eq!(
        roles("[18:35:40] [uploaded] int_loyalty/cartridge/scripts/constants.js"),
        vec![
            (Role::Timestamp, "[18:35:40]".to_string()),
            (Role::Plain, " ".to_string()),
            (Role::Uploaded, "[uploaded]".to_string()),
            (Role::Plain, " ".to_string()),
            (Role::Cartridge, "int_loyalty".to_string()),
            (Role::Folder, "/cartridge/scripts/".to_string()),
            (Role::File, "constants.js".to_string()),
        ]
    );
}

#[test]
fn a_root_file_keeps_a_lone_slash_as_its_folder() {
    let got = roles("[09:00:00] [uploaded] app_brand/dw.json");
    assert!(got.contains(&(Role::Cartridge, "app_brand".to_string())));
    assert!(got.contains(&(Role::Folder, "/".to_string())));
    assert!(got.contains(&(Role::File, "dw.json".to_string())));
}

#[test]
fn a_grouped_headline_is_not_mistaken_for_a_path() {
    let got = roles("[18:02:10] [uploaded] 7 file(s) in 2 cartridge(s)");
    assert!(got.contains(&(Role::Uploaded, "[uploaded]".to_string())));
    assert!(got.contains(&(Role::Plain, "7 file(s) in 2 cartridge(s)".to_string())));
    assert!(!got.iter().any(|(role, _)| *role == Role::Cartridge));
}

#[test]
fn an_error_line_is_red_from_the_badge_to_the_end() {
    let got = roles("[18:10:45] [error]    cannot read file.js: permission denied");
    assert!(got.contains(&(Role::Error, "[error]".to_string())));
    assert!(got.contains(&(Role::Error, "cannot read file.js: permission denied".to_string())));
}

#[test]
fn ok_and_warn_markers_keep_their_tone() {
    assert_eq!(roles("[10:00:00] [ok]       done")[2], (Role::Ok, "[ok]".to_string()));
    assert_eq!(
        roles("[10:00:00] [warn]     careful")[2],
        (Role::Warn, "[warn]".to_string())
    );
}

#[test]
fn any_other_bracketed_word_is_still_a_tag() {
    let line = "[10:00:00] [queued]   47 changes";
    assert_eq!(roles(line)[2], (Role::Tag, "[queued]".to_string()));
    assert_eq!(rejoined(line), line);
}

#[test]
fn a_line_without_a_timestamp_stays_whole_and_plain() {
    assert_eq!(
        roles("             cartridge/scripts  vouchers.js"),
        vec![(Role::Plain, "             cartridge/scripts  vouchers.js".to_string())]
    );
}
