use super::*;

/// The layout, never the colour: every expectation below is plain text.
fn plain(change: Change, paths: &[String], headline: bool) -> Vec<String> {
    no_color_in_tests();
    render(change, paths, headline)
}

fn sorted(raw: &[&str]) -> Vec<String> {
    let mut paths: Vec<String> = raw.iter().map(|path| path.to_string()).collect();
    paths.sort();
    paths
}

#[test]
fn a_single_file_stays_on_the_header_line() {
    let lines = plain(Change::Uploaded, &sorted(&["app_brand/cartridge/js/checkout.js"]), true);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].ends_with("app_brand/cartridge/js/checkout.js uploaded"));
}

#[test]
fn files_group_under_their_cartridge_and_folder() {
    let lines = render(
        Change::Uploaded,
        &sorted(&[
            "app_brand/cartridge/js/checkout.js",
            "app_brand/cartridge/js/cart.js",
            "app_brand/cartridge/templates/billing.isml",
            "int_rewards/cartridge/scripts/vouchers.js",
        ]),
        true,
    );

    assert!(lines[0].contains("4 file(s) uploaded in 2 cartridge(s)"));
    assert_eq!(lines[1].trim(), "app_brand");
    assert_eq!(lines[2].trim(), "cartridge/js         cart.js  checkout.js");
    assert_eq!(lines[3].trim(), "cartridge/templates  billing.isml");
    assert_eq!(lines[4].trim(), "int_rewards");
    assert_eq!(lines[5].trim(), "cartridge/scripts  vouchers.js");
}

#[test]
fn a_folder_too_long_to_align_takes_its_own_line() {
    let deep = format!("app_brand/{}/file.js", "a".repeat(FOLDER_COLUMN + 5));
    let lines = plain(Change::Uploaded, &sorted(&[&deep, "app_brand/cartridge/js/cart.js"]), true);

    assert_eq!(lines[2].trim(), "a".repeat(FOLDER_COLUMN + 5));
    assert_eq!(lines[3].trim(), "file.js");
}

#[test]
fn long_file_lists_collapse_into_a_count() {
    let many: Vec<String> = (0..MAX_NAMES + 4)
        .map(|index| format!("app_brand/cartridge/js/file{index}.js"))
        .collect();
    let lines = plain(Change::Uploaded, &many, true);

    assert!(lines.last().expect("rendered").trim().ends_with("+4 more"));
}

#[test]
fn deleting_a_whole_cartridge_prints_its_name() {
    let lines = plain(Change::Deleted, &sorted(&["app_brand", "int_rewards"]), true);

    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("2 path(s) deleted in 2 cartridge(s)"));
    assert_eq!(lines[1].trim(), "app_brand");
    assert_eq!(lines[2].trim(), "int_rewards");
}

#[test]
fn nothing_to_report_prints_nothing() {
    assert!(plain(Change::Uploaded, &[], true).is_empty());
}

#[test]
fn a_listing_leaves_the_headline_to_its_caller() {
    let lines = plain(Change::Uploaded, &sorted(&["app_brand/cartridge/js/cart.js"]), false);

    assert_eq!(lines[0].trim(), "app_brand");
    assert_eq!(lines[1].trim(), "cartridge/js  cart.js");
}

#[test]
fn the_colour_switch_obeys_the_user_before_the_terminal() {
    assert!(wants_color("always"));
    assert!(!wants_color("never"));
}

#[test]
fn a_tone_wraps_the_text_and_no_tone_leaves_it_alone() {
    assert_eq!(tint(GREEN, "up"), format!("{GREEN}up{RESET}"));
    assert_eq!(tint("", "up"), "up");
}

#[test]
fn names_wrap_instead_of_running_off_the_screen() {
    let names: Vec<&str> = vec!["aaaa", "bbbb", "cccc"];
    assert_eq!(wrap(&names, 12), vec!["aaaa  bbbb", "cccc"]);
    assert_eq!(wrap(&names, 80), vec!["aaaa  bbbb  cccc"]);
}
