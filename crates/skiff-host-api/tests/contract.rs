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

#[test]
fn m1a_status_is_additive_and_gated_by_its_own_capability() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1a.json")).unwrap();
    let request: Request = serde_json::from_value(fixture["request"].clone()).unwrap();
    let response: Response = serde_json::from_value(fixture["reply"].clone()).unwrap();
    let capability: Capability = serde_json::from_value(fixture["capability"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), fixture["request"]);
    assert_eq!(serde_json::to_value(&response).unwrap(), fixture["reply"]);
    assert_eq!(request.capability(), Some(capability));
    assert!(request.accepts(&response));
    assert!(!request.accepts(&Response::Ok));
    assert!(!Request::ListSessions.accepts(&response));
}

#[test]
fn m1b_screen_has_an_independent_read_only_capability_and_lossless_golden_shape() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1b.json")).unwrap();
    let request: Request = serde_json::from_value(fixture["request"].clone()).unwrap();
    let reply: Response = serde_json::from_value(fixture["reply"].clone()).unwrap();
    let error: Error = serde_json::from_value(fixture["error"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), fixture["request"]);
    assert_eq!(serde_json::to_value(&reply).unwrap(), fixture["reply"]);
    assert_eq!(serde_json::to_value(error).unwrap(), fixture["error"]);
    assert_eq!(
        serde_json::to_value(request.capability().unwrap()).unwrap(),
        fixture["capability"]
    );
    assert!(request.accepts(&reply));
    assert!(!request.accepts(&Response::Ok));
    assert!(!Request::ListSessions.accepts(&reply));
    let envelope = Envelope { id: 9, request };
    assert_eq!(
        serde_json::from_value::<Envelope>(serde_json::to_value(&envelope).unwrap()).unwrap(),
        envelope
    );
}

#[test]
fn m1c_inventory_golden_schema_preserves_defaults_nulls_omissions_and_metadata() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1c.json")).unwrap();
    let request: Request = serde_json::from_value(fixture["request"].clone()).unwrap();
    let response: Response = serde_json::from_value(fixture["reply"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), fixture["request"]);
    assert_eq!(serde_json::to_value(&response).unwrap(), fixture["reply"]);
    assert_eq!(
        serde_json::to_value(request.capability().unwrap()).unwrap(),
        fixture["capability"]
    );
    assert!(request.accepts(&response));
    assert!(!request.accepts(&Response::Ok));
    assert!(!Request::ListSessions.accepts(&response));
    let minimal: InventorySession =
        serde_json::from_value(fixture["legacy_minimal"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(minimal).unwrap(),
        fixture["minimal_output"]
    );
    let mut malformed = fixture["legacy_minimal"].clone();
    malformed["role"] = "future-role".into();
    assert!(serde_json::from_value::<InventorySession>(malformed).is_err());
    let mut malformed = fixture["reply"]["sessions"][0].clone();
    malformed["watch"]["pids"] = json!(["bad"]);
    assert!(serde_json::from_value::<InventorySession>(malformed).is_err());
}

#[test]
fn m1d_session_updates_have_an_explicit_capability_envelope_and_golden_shape() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1d.json")).unwrap();
    assert_eq!(
        serde_json::to_value(Capability::session_updates()).unwrap(),
        fixture["capability"]
    );
    for value in fixture["messages"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
        let Message::Event { event } = message else { panic!("not an event") };
        assert_eq!(event.capability(), Capability::session_updates());
    }
    // M0 event families keep their capability; unknown kinds and states fail.
    let m0: Event = serde_json::from_value(json!({"event":"output","session":"s","data":""})).unwrap();
    assert_eq!(m0.capability(), Capability::sessions());
    for update in [
        json!({"kind":"future_update","session":"a"}),
        json!({"kind":"state","session":"a","state":"future"}),
        json!({"kind":"exit","session":"a","code":"x"}),
        json!({"session":"a"}),
    ] {
        let event = json!({"event":"session_update","update":update});
        assert!(serde_json::from_value::<Event>(event).is_err());
    }
}
