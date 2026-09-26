use super::*;
use sfcc_core::logs::parse_entries;

fn one(file: &str, text: &str) -> Signature {
    let entries = parse_entries(file, text);
    assert_eq!(entries.len(), 1, "the fixture is one record");
    signature(&entries[0])
}

const WRAPPED: &str = concat!(
    "[2026-09-22 21:38:04.112 GMT] ERROR PipelineCallServlet|157318437|Sites-AcmeEU-Site|Checkout-Begin|PipelineCall|t1ZZ-bCbTd custom.checkout [] ",
    "Error while executing script 'app_acme/cartridge/controllers/Checkout.js': Wrapped com.demandware.beehive.core.capi.pipeline.PipelineExecutionException: ",
    "TypeError: Cannot read property \"shipments\" from null (app_acme/cartridge/scripts/checkout/CheckoutServices.js#214)\n",
    "\tat app_acme/cartridge/scripts/checkout/CheckoutServices.js:214 (validateBasket)\n",
    "\tat app_acme/cartridge/controllers/Checkout.js:88 (anonymous)\n",
    "\tat modules/server/route.js:83 (next)\n",
    "System Information\n",
    "------------------\n",
    "RequestID: bf2c1a9e7d1d4c0c8a5c3f35e1\n",
    "customer: jane.doe@example.com\n",
);

#[test]
fn reads_what_failed_and_where() {
    let signature = one("error-blade1-4-appserver-20260922.log", WRAPPED);

    assert_eq!(signature.label, "error");
    assert_eq!(signature.exception_class.as_deref(), Some("TypeError"));
    assert_eq!(
        signature.location.as_deref(),
        Some("app_acme/cartridge/scripts/checkout/CheckoutServices.js:214")
    );
    assert_eq!(signature.frames.len(), 3);
    assert_eq!(signature.id.len(), 16);
}

#[test]
fn the_thread_keeps_the_site_and_the_controller_but_not_the_session() {
    let signature = one("error-blade1-20260922.log", WRAPPED);

    assert!(
        signature
            .message
            .starts_with("ERROR PipelineCallServlet|Sites-AcmeEU-Site|Checkout-Begin|PipelineCall custom.checkout"),
        "{}",
        signature.message
    );
    assert!(!signature.message.contains("157318437"));
    assert!(!signature.message.contains("t1ZZ-bCbTd"));
}

#[test]
fn the_request_dump_after_the_stack_never_reaches_the_signature() {
    let signature = one("error-blade1-20260922.log", WRAPPED);
    let example = signature.example();

    assert!(!example.contains("jane.doe"));
    assert!(!example.contains("RequestID"));
}

#[test]
fn another_occurrence_of_the_same_failure_has_the_same_signature() {
    let again = WRAPPED
        .replace("21:38:04.112", "23:01:59.870")
        .replace("157318437", "99102")
        .replace("t1ZZ-bCbTd", "Qp7s-xxYzA");

    assert_eq!(
        one("error-blade1-20260922.log", WRAPPED).id,
        one("error-blade2-20260923.log", &again).id
    );
}

#[test]
fn moving_lines_around_in_the_file_keeps_the_signature() {
    let shifted = WRAPPED.replace("#214", "#230").replace(":214", ":230");

    let before = one("error-blade1-20260922.log", WRAPPED);
    let after = one("error-blade1-20260922.log", &shifted);

    assert_eq!(before.id, after.id);
    assert_ne!(before.location, after.location);
}

#[test]
fn a_different_function_is_a_different_failure() {
    let elsewhere = WRAPPED.replace("(validateBasket)", "(calculateTotals)");

    assert_ne!(
        one("error-blade1-20260922.log", WRAPPED).id,
        one("error-blade1-20260922.log", &elsewhere).id
    );
}

#[test]
fn the_level_is_part_of_the_signature() {
    assert_ne!(
        one("error-blade1-20260922.log", WRAPPED).id,
        one("customerror-blade1-20260922.log", WRAPPED).id
    );
}

#[test]
fn scrubs_ids_order_numbers_and_personal_data() {
    let scrubbed = scrub(
        "Order 00012345 for jane.doe@example.com failed: product M0E20000000DZA0 \
         at 12.99 EUR, basket 3f2a7c1e-5b8d-4c1e-9a7f-2b6c8d9e0f1a from 10.2.3.4 \
         (dwsid=Zx9-abc_DEF) Authorization: Bearer eyJhbGciOiJIUzI1NiJ9",
    );

    for leaked in [
        "00012345",
        "jane.doe",
        "M0E20000000DZA0",
        "12.99",
        "3f2a7c1e",
        "10.2.3.4",
        "Zx9-abc_DEF",
        "eyJhbGciOiJIUzI1NiJ9",
    ] {
        assert!(!scrubbed.contains(leaked), "{leaked} survived: {scrubbed}");
    }
    assert!(
        scrubbed.contains("Order <n> for <email> failed"),
        "{scrubbed}"
    );
}

#[test]
fn keeps_what_makes_a_message_mean_something() {
    let scrubbed = scrub(
        "HTTP 404 calling service int_payment.http.psp: Cannot read property \"ID\" of undefined in app_acme_v2/cartridge/scripts/x.js",
    );

    assert!(scrubbed.contains("HTTP 404"));
    assert!(scrubbed.contains("\"ID\""));
    assert!(scrubbed.contains("app_acme_v2/cartridge/scripts/x.js"));
}

#[test]
fn a_url_keeps_its_host_and_path_but_not_its_query() {
    let scrubbed =
        scrub("GET https://api.example.com/v1/orders/00098765?token=abc123&email=a@b.com failed");

    assert_eq!(scrubbed, "GET https://api.example.com/v1/orders/<n> failed");
}

#[test]
fn a_record_without_a_stack_is_placed_by_the_script_its_message_names() {
    let signature = one(
        "customerror-blade1-20260922.log",
        "[2026-09-22 10:00:00.000 GMT] ERROR PipelineCallServlet|1|Sites-X-Site|Cart-Show|PipelineCall|abc custom.cart [] \
         Wrapped java.lang.NullPointerException (app_x/cartridge/scripts/cart/cartHelpers.js#51)",
    );

    assert_eq!(
        signature.exception_class.as_deref(),
        Some("NullPointerException")
    );
    assert_eq!(
        signature.location.as_deref(),
        Some("app_x/cartridge/scripts/cart/cartHelpers.js:51")
    );
}

#[test]
fn a_line_that_is_not_a_record_header_is_still_signed() {
    let signature = one(
        "error-blade1-20260922.log",
        "\tat modules/server/route.js:83",
    );
    assert_eq!(signature.id.len(), 16);
}

#[test]
fn an_expression_in_a_template_is_placed_at_the_template_not_at_render_js() {
    let wrapper = one(
        "error-blade1-20260922.log",
        "[2026-09-22 10:00:00.000 GMT] ERROR PipelineCallServlet|1|Sites-X-Site|Account-EditProfile|PipelineCall|abc org.apache.jsp._x.cartridges._y.default_.account._z Sites-X-Site STOREFRONT a b 1 - Error in template script.\n\
         \tat [Template:account/editProfileForm:${pdict.profileForm.customer.base.apply.htmlName}]:1\n\
         \tat modules/server/render.js:22 (template)\n\
         \tat modules/server/render.js:102 (anonymous)\n",
    );
    assert_eq!(
        wrapper.location.as_deref(),
        Some("account/editProfileForm.isml")
    );

    let cause = one(
        "customerror-blade1-20260922.log",
        "[2026-09-22 10:00:00.000 GMT] ERROR PipelineCallServlet|1|Sites-X-Site|Search-Show|PipelineCall|abc custom.isml [] TypeError: Cannot read property \"isCategorySearch\" from null\n\
         \tat [Template:/search/searchResultsNoDecorator:${pdict.productSearch.isCategorySearch ? 'a' : 'b'}]:1\n\
         \tat modules/server/render.js:22 (template)\n",
    );
    assert_eq!(
        cause.location.as_deref(),
        Some("search/searchResultsNoDecorator.isml")
    );
}
