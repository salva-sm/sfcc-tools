use super::active_in;

#[test]
fn the_active_code_version_is_the_one_marked_so() {
    let body = r#"{"_v":"23.2","count":2,"data":[
        {"_type":"code_version","active":false,"id":"version1"},
        {"_type":"code_version","active":true,"id":"release_42"}
    ],"total":2}"#;
    assert_eq!(active_in(body).unwrap().as_deref(), Some("release_42"));
}

#[test]
fn a_sandbox_can_have_none_active() {
    let body = r#"{"count":1,"data":[{"active":false,"id":"version1"}],"total":1}"#;
    assert_eq!(active_in(body).unwrap(), None);
}

#[test]
fn a_list_without_data_is_an_error() {
    assert!(active_in(r#"{"fault":{"type":"InvalidAccessTokenException"}}"#).is_err());
}
