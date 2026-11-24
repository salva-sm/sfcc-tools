use super::*;

fn record(level: Option<&str>, controller: Option<&str>, message: &str) -> Option<Record> {
    Some(Record {
        level: level.map(str::to_string),
        controller: controller.map(str::to_string),
        message: message.to_string(),
    })
}

#[test]
fn a_request_yields_its_level_controller_and_message() {
    let line = "[2026-11-24 08:26:31.776 GMT] WARN PipelineCallServlet|732326893|Sites-guess-Site|Account-Show|PipelineCall|Qo1sDWQWTJ custom [] Content asset with ID Account_MyGUESS_benefitsFreeShipping is not online";
    assert_eq!(
        clean(line),
        record(
            Some("WARN"),
            Some("Account-Show"),
            "Content asset with ID Account_MyGUESS_benefitsFreeShipping is not online"
        )
    );
}

#[test]
fn a_dotted_logger_and_its_mdc_are_dropped_from_the_message() {
    let line = "[2026-11-24 10:00:00.000 GMT] ERROR PipelineCallServlet|9|Sites-guess-Site|Cart-AddProduct|PipelineCall|-abc123 custom.service.COMM [req-7] inventory record not found";
    assert_eq!(
        clean(line),
        record(Some("ERROR"), Some("Cart-AddProduct"), "inventory record not found")
    );
}

#[test]
fn a_system_line_takes_its_message_after_the_dash() {
    let line = "[2026-11-24 10:00:00.000 GMT] INFO PipelineCallServlet|5|Sites-guess-Site|Product-Show|PipelineCall|-z9 system.core STOREFRONT - rendering pdp";
    assert_eq!(clean(line), record(Some("INFO"), Some("Product-Show"), "rendering pdp"));
}

#[test]
fn a_job_line_stands_its_job_in_for_the_controller() {
    let line = "[SystemJobThread|732|job-123|ProcessOrdersJob] starting the export";
    assert_eq!(clean(line), record(None, Some("ProcessOrdersJob"), "starting the export"));
}

#[test]
fn a_request_without_a_controller_still_cleans() {
    let line = "[2026-11-24 10:00:00.000 GMT] DEBUG PipelineCallServlet|9|Sites-guess-Site|-xZ12ab custom.core hello";
    assert_eq!(clean(line), record(Some("DEBUG"), None, "hello"));
}

#[test]
fn a_thread_log_with_no_request_head_still_cleans() {
    let line = "[2026-11-24 09:58:38.177 GMT] DEBUG RepeatedMessageSuppressingFilter-Thread custom.hooks [] The following message was generated";
    assert_eq!(
        clean(line),
        record(Some("DEBUG"), None, "The following message was generated")
    );
}

#[test]
fn lines_that_are_not_records_are_left_for_the_caller() {
    for line in [
        "\tat app_brand/cartridge/controllers/Account.js:99 (anonymous)",
        "TypeError: Cannot read property \"ID\" from null",
        "following 3 log file(s) on bdzv-003",
        "[2026-11-24 10:00:00.000 GMT] DEBUG some|piped|text with no logger after",
    ] {
        assert_eq!(clean(line), None, "{line}");
    }
}
