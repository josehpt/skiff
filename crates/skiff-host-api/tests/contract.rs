use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use skiff_host_api::*;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/m0.json")).unwrap()
}
fn golden<T: DeserializeOwned + Serialize>(name: &str) {
    for value in fixture()[name].as_array().unwrap() {
        let decoded: T = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), *value, "{name}");
    }
}

#[test]
fn every_m0_shape_matches_golden_json() {
    golden::<Host>("hosts");
    golden::<Endpoint>("endpoints");
    golden::<Hello>("hellos");
    golden::<Capability>("capabilities");
    golden::<Limits>("limits");
    golden::<Session>("sessions");
    golden::<Request>("requests");
    golden::<Envelope>("envelopes");
    golden::<Response>("responses");
    golden::<Event>("events");
    golden::<Error>("errors");
    golden::<Message>("messages");
}

#[test]
fn binary_bytes_are_base64_not_utf8_and_order_survives() {
    let r: Request = serde_json::from_value(fixture()["requests"][2].clone()).unwrap();
    assert_eq!(
        r,
        Request::Write {
            session: "s1".into(),
            data: vec![0, 255, 10, 13],
            paste: false
        }
    );
    for key in ["snapshot", "output"] {
        let e = json!({"event":key,"session":"s1","data":"not base64!","cols":80,"rows":24});
        assert!(serde_json::from_value::<Event>(e).is_err());
    }
}

#[test]
fn additive_metadata_and_future_capabilities_do_not_break_old_readers() {
    let mut value = fixture()["hellos"][0].clone();
    value["future_optional"] = json!({"anything":true});
    value["capabilities"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"future.feature","version":3}));
    let hello: Hello = serde_json::from_value(value).unwrap();
    hello.validate().unwrap();
    hello.require(&Capability::sessions()).unwrap();
    assert!(matches!(
        hello.require(&Capability {
            name: "files.read".into(),
            version: 1
        }),
        Err(Error::UnsupportedCapability { .. })
    ));
    let session: Session =
        serde_json::from_value(json!({"id":"s","cwd":"/x","label":"x","new":42})).unwrap();
    assert_eq!(session.generation, None);
}

#[test]
fn incompatible_major_invalid_identity_and_bad_limits_are_explicit() {
    let mut hello: Hello = serde_json::from_value(fixture()["hellos"][0].clone()).unwrap();
    hello.api_major = 2;
    assert_eq!(
        hello.validate(),
        Err(Error::IncompatibleVersion {
            supported: 1,
            received: 2
        })
    );
    hello.api_major = 1;
    hello.limits.max_in_flight = 0;
    assert!(matches!(
        hello.validate(),
        Err(Error::MalformedContract { .. })
    ));
    assert!(serde_json::from_value::<HostId>(json!("")).is_err());
    assert!(serde_json::from_value::<InstanceId>(json!(" ")).is_err());
    assert!(serde_json::from_value::<Hello>(json!({"api_major":1})).is_err());
}

#[test]
fn capability_majors_are_exact_and_service_versions_do_not_replace_negotiation() {
    let mut hello: Hello = serde_json::from_value(fixture()["hellos"][0].clone()).unwrap();
    hello.capabilities[0].version = 2;
    hello
        .service_versions
        .insert("skiff-host".into(), "900.0.0".into());
    hello.validate().unwrap();
    assert!(matches!(
        hello.require(&Capability::sessions()),
        Err(Error::UnsupportedCapability { .. })
    ));
}
