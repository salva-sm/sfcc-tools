use super::{Config, Credentials, Instance, classify_host};
use std::path::PathBuf;

#[test]
fn recognises_developer_sandboxes() {
    assert_eq!(classify_host("sbx-001.my.commercecloud.salesforce.com"), Instance::Sandbox);
    assert_eq!(classify_host("sbx-001.dx.commercecloud.salesforce.com"), Instance::Sandbox);
}

#[test]
fn recognises_shared_instances() {
    assert_eq!(classify_host("production-eu01-acme.demandware.net"), Instance::Production);
    assert_eq!(classify_host("staging-eu01-acme.demandware.net"), Instance::Staging);
    assert_eq!(classify_host("development-eu01-acme.demandware.net"), Instance::Development);
    assert_eq!(classify_host("intranet.acme.example"), Instance::Unknown);
}

#[test]
fn does_not_confuse_a_realm_containing_the_word() {
    assert_eq!(classify_host("prdz-001.my.commercecloud.salesforce.com"), Instance::Sandbox);
}

#[test]
fn reads_a_dw_json_the_way_prophet_writes_it() {
    let home = scratch("dwjson-basic");
    let config = Config::load(Some(home.join("dw.json")), None).expect("dw.json should load");

    assert_eq!(config.hostname, "sbx-001.my.commercecloud.salesforce.com");
    assert_eq!(config.code_version, "version1");
    assert!(config.cartridges_dir.ends_with("cartridges"));
    assert!(config.cartridges_dir.join("app_x").join("cartridge").is_dir());
    assert!(matches!(config.credentials, Credentials::Basic { .. }));
    assert!(config.api_client.is_none());

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn takes_the_api_client_from_the_sfcc_ci_block() {
    let home = scratch("dwjson-apiclient");
    std::fs::write(
        home.join("dw.json"),
        r#"{
            "hostname": "sbx-001.my.commercecloud.salesforce.com",
            "username": "someone",
            "password": "secret",
            "custom-sfcc-ci": {
                "sfcc-oauth-client-id": "the-id",
                "sfcc-oauth-client-secret": "the-secret"
            }
        }"#,
    )
    .unwrap();

    let config = Config::load(Some(home.join("dw.json")), Some("version9".into())).unwrap();
    let api_client = config.api_client.expect("the block should be picked up");

    assert_eq!(api_client.id, "the-id");
    assert_eq!(api_client.secret, "the-secret");
    assert_eq!(config.code_version, "version9");

    let _ = std::fs::remove_dir_all(&home);
}

fn scratch(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("prost-test-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("cartridges").join("app_x").join("cartridge")).unwrap();
    std::fs::write(
        home.join("dw.json"),
        r#"{
            "hostname": "sbx-001.my.commercecloud.salesforce.com",
            "username": "someone",
            "password": "secret"
        }"#,
    )
    .unwrap();
    home
}
