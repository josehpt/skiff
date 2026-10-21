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
        let Message::Event { event } = message else {
            panic!("not an event")
        };
        assert_eq!(event.capability(), Capability::session_updates());
    }
    // M0 event families keep their capability; unknown kinds and states fail.
    let m0: Event =
        serde_json::from_value(json!({"event":"output","session":"s","data":""})).unwrap();
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

#[test]
fn m1e_session_terminal_has_its_own_capability_and_lossless_golden_shape() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1e.json")).unwrap();
    assert_eq!(
        serde_json::to_value(Capability::session_terminal()).unwrap(),
        fixture["capability"]
    );
    for value in fixture["requests"].as_array().unwrap() {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), *value);
        assert_eq!(request.capability(), Some(Capability::session_terminal()));
        // Every terminal request is acknowledged with Ok, never another reply.
        assert!(request.accepts(&Response::Ok));
        assert!(!request.accepts(&Response::SessionScreen { lines: vec![] }));
    }
    let input: Request = serde_json::from_value(fixture["requests"][3].clone()).unwrap();
    assert!(
        matches!(input, Request::TerminalInput { data, paste: false, .. } if data == [0, 255, 10, 13, 27])
    );
    // `paste` defaults to false, as M0 Write does.
    let typed: Request =
        serde_json::from_value(json!({"cmd":"terminal_input","session":"s","data":"eA=="}))
            .unwrap();
    assert!(matches!(typed, Request::TerminalInput { paste: false, .. }));
    for value in fixture["messages"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
        if let Message::Event { event } = message {
            assert_eq!(event.capability(), Capability::session_terminal());
            assert!(event.into_terminal().is_some_and(|(s, _)| s == "s1"));
        }
    }
    let output: Event = serde_json::from_value(fixture["messages"][3]["event"].clone()).unwrap();
    assert_eq!(
        output.into_terminal(),
        Some((
            "s1".into(),
            TerminalFrame::Output {
                data: b"ls\n".to_vec(),
                echo: Some(EchoTimes {
                    wrote: u64::MAX,
                    read: 0
                })
            }
        ))
    );
    // M0 families keep sessions@1 and are not terminal frames; bad bytes fail.
    let m0: Event =
        serde_json::from_value(json!({"event":"output","session":"s","data":""})).unwrap();
    assert_eq!(m0.capability(), Capability::sessions());
    assert!(m0.into_terminal().is_none());
    let m0: Request = serde_json::from_value(json!({"cmd":"subscribe","session":"s"})).unwrap();
    assert_eq!(m0.capability(), Some(Capability::sessions()));
    for bad in [
        json!({"event":"terminal_output","session":"s","data":"not base64!"}),
        json!({"event":"terminal_snapshot","session":"s","data":"","cols":-1,"rows":0}),
        json!({"event":"terminal_snapshot","session":"s","data":""}),
    ] {
        assert!(serde_json::from_value::<Event>(bad).is_err());
    }
    assert!(serde_json::from_value::<Request>(
        json!({"cmd":"terminal_resize","session":"s","cols":65536,"rows":1})
    )
    .is_err());
    // Only a local endpoint may be managed (spawn, signal, reload, link).
    assert!(Endpoint::Local {
        socket: "/s".into()
    }
    .is_local());
    assert!(!Endpoint::Ssh {
        hostname: "h".into(),
        account: "a".into(),
        port: 22,
        socket: "/s".into()
    }
    .is_local());
}

#[test]
fn m1g_lifecycle_and_groups_have_their_own_capabilities_and_lossless_golden_shapes() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1g.json")).unwrap();
    let lifecycle = Capability::session_lifecycle();
    let groups = Capability::session_groups();
    assert_eq!(
        serde_json::to_value(&lifecycle).unwrap(),
        fixture["capabilities"]["lifecycle"]
    );
    assert_eq!(
        serde_json::to_value(&groups).unwrap(),
        fixture["capabilities"]["groups"]
    );
    let row = Response::Session {
        session: Box::new(
            serde_json::from_value(fixture["replies"][0]["result"]["Ok"]["session"].clone())
                .unwrap(),
        ),
    };
    let group: Group =
        serde_json::from_value(fixture["replies"][2]["result"]["Ok"]["group"].clone()).unwrap();
    for (key, capability) in [
        ("lifecycle_requests", &lifecycle),
        ("group_requests", &groups),
    ] {
        for value in fixture[key].as_array().unwrap() {
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&request).unwrap(), *value);
            assert_eq!(request.capability().as_ref(), Some(capability));
            // Each request takes exactly one reply shape; Ok never stands in for a row.
            let wants_row = matches!(
                request,
                Request::SessionCreate { .. }
                    | Request::SessionRename { .. }
                    | Request::SessionSetTheme { .. }
            );
            assert_eq!(request.accepts(&row), wants_row, "{value}");
            assert_eq!(
                request.accepts(&Response::Groups { groups: vec![] }),
                matches!(request, Request::GroupList)
            );
            assert_eq!(
                request.accepts(&Response::Group {
                    group: group.clone()
                }),
                matches!(request, Request::GroupSave { .. })
            );
            assert_eq!(
                request.accepts(&Response::Ok),
                matches!(
                    request,
                    Request::SessionKill { .. }
                        | Request::SessionSeen { .. }
                        | Request::GroupDelete { .. }
                )
            );
        }
    }
    for value in fixture["replies"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
    }
    // Spec defaults match the legacy SessionSpec: shell role, 120×40, no peek.
    let minimal: Request =
        serde_json::from_value(json!({"cmd":"session_create","spec":{}})).unwrap();
    assert_eq!(
        serde_json::to_value(&minimal).unwrap(),
        json!({"cmd":"session_create","spec":{"label":"","name":null,"role":"shell","cwd":null,"command":null,"args":[],"peek":false,"cols":120,"rows":40}})
    );
    // A coordinator flag is not part of the contract; unknown fields are ignored.
    let ignored: Request =
        serde_json::from_value(json!({"cmd":"session_create","spec":{"coordinator":true}}))
            .unwrap();
    assert_eq!(ignored, minimal);
    for bad in [
        json!({"cmd":"session_create","spec":{"role":"robot"}}),
        json!({"cmd":"session_create","spec":{"cols":-1}}),
        json!({"cmd":"session_kill"}),
        json!({"cmd":"group_save","group":{"name":"x","layout":{"type":"tab"}}}),
        json!({"cmd":"group_save","group":{"name":"x","layout":{"type":"split","dir":"diagonal","ratio":0.5,"a":{"type":"pane","session":"a"},"b":{"type":"pane","session":"b"}}}}),
    ] {
        assert!(
            serde_json::from_value::<Request>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    // Older contract families keep their own capabilities.
    let m0: Request = serde_json::from_value(json!({"cmd":"list_sessions"})).unwrap();
    assert_eq!(m0.capability(), Some(Capability::sessions()));
}

#[test]
fn m3_agent_list_has_its_own_read_only_capability_and_lossless_golden_shape() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m3.json")).unwrap();
    let agents = Capability::session_agents();
    assert_eq!(
        serde_json::to_value(&agents).unwrap(),
        fixture["capabilities"]["agents"]
    );
    let list = Response::Agents { agents: vec![] };
    for value in fixture["agent_requests"].as_array().unwrap() {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), *value);
        assert_eq!(request.capability(), Some(agents.clone()));
        // Exactly one reply shape: never Ok, a row or groups.
        assert!(request.accepts(&list));
        assert!(!request.accepts(&Response::Ok));
        assert!(!request.accepts(&Response::Groups { groups: vec![] }));
    }
    for value in fixture["replies"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
    }
    // No other request is answered by an agent list.
    let kill: Request = serde_json::from_value(json!({"cmd":"session_kill","session":"s"})).unwrap();
    assert!(!kill.accepts(&list));
    // The list grants nothing else: lifecycle and groups keep their own.
    assert_ne!(kill.capability(), Some(agents.clone()));
    // A row needs its id, command and states; extra fields are ignored.
    for bad in [
        json!({"kind":"agents","agents":[{"id":"x","command":"x","installed":true}]}),
        json!({"kind":"agents","agents":[{"command":"x","installed":true,"enabled":true}]}),
        json!({"kind":"agents"}),
    ] {
        assert!(
            serde_json::from_value::<Response>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn m4_projects_and_git_status_are_read_only_data_plane_families() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m4.json")).unwrap();
    let projects = Capability::projects();
    let git = Capability::git_status();
    assert_eq!(serde_json::to_value(&projects).unwrap(), fixture["capabilities"]["projects"]);
    assert_eq!(serde_json::to_value(&git).unwrap(), fixture["capabilities"]["git_status"]);
    let list = Response::Projects { projects: vec![] };
    let changes = Response::Changes { changes: vec![] };
    for (key, cap) in [("project_requests", &projects), ("git_requests", &git)] {
        for value in fixture[key].as_array().unwrap() {
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&request).unwrap(), *value);
            assert_eq!(request.capability().as_ref(), Some(cap));
            // Exactly one reply kind each, never Ok.
            assert!(!request.accepts(&Response::Ok));
            assert_eq!(request.accepts(&changes), cap == &git);
        }
    }
    assert!(Request::ProjectList.accepts(&list));
    assert!(!Request::ProjectGet { name: "x".into() }.accepts(&list));
    for value in fixture["replies"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
    }
    // Neither family grants a session operation, nor the other family.
    for session in [Request::SessionInventory, Request::AgentList, Request::GroupList] {
        let c = session.capability().unwrap();
        assert!(c != projects && c != git);
    }
    assert_ne!(projects, git);
    // A row needs its states; a status is one of four letters.
    for bad in [
        json!({"kind":"changes","changes":[{"path":"a","status":"X","added":1,"removed":0}]}),
        json!({"kind":"changes","changes":[{"path":"a","added":1,"removed":0}]}),
        json!({"kind":"projects","projects":[{"name":"x","short":"x","path":"/x","color":null,"agents":[],"worktrees":[],"error":null}]}),
        json!({"kind":"project","project":{"name":"x"}}),
    ] {
        let ok = serde_json::from_value::<Response>(bad.clone()).is_ok();
        // A project with only the required fields decodes: the rest default.
        assert_eq!(ok, bad["kind"] == "projects", "{bad}");
    }
}
