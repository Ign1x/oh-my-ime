use super::*;
use omime_proto::manifest::Manifest;

#[derive(Default)]
struct TestFrontends {
    finished: Vec<(KeyRef, bool)>,
    actions: Vec<Action>,
}

impl Frontends for TestFrontends {
    fn finish_key(&mut self, key: &KeyRef, handled: bool) {
        self.finished.push((*key, handled));
    }

    fn apply_actions(
        &mut self,
        _origin: Origin,
        _ctx: ContextId,
        actions: &[Action],
        _reply_key: Option<&KeyRef>,
    ) {
        self.actions.extend_from_slice(actions);
    }

    fn forward_key(
        &mut self,
        _origin: Origin,
        _ctx: ContextId,
        _key: &crate::frontend::ForwardedKey,
    ) {
        panic!("these native-engine cases do not forward keys asynchronously");
    }

    fn clear_preedit(&mut self, _origin: Origin, _ctx: ContextId) {}

    fn set_active_engine(&mut self, _engine: Option<EngineId>) {}

    fn set_engine_visible(&mut self, _engine: EngineId, _visible: bool) {}

    fn hosted_activate(
        &mut self,
        _engine: &EngineId,
        _origin: Origin,
        _ctx: ContextId,
        _info: &ContextInfo,
    ) -> bool {
        panic!("these cases use a native engine");
    }

    fn hosted_update(
        &mut self,
        _engine: &EngineId,
        _ctx: ContextId,
        _info: &ContextInfo,
        _reset: bool,
    ) {
        panic!("these cases use a native engine");
    }

    fn hosted_deactivate(&mut self, _engine: &EngineId, _ctx: ContextId) {
        panic!("these cases use a native engine");
    }

    fn hosted_key(&mut self, _engine: &EngineId, _key: &KeyRef, _event: &KeyEvent) -> KeyDelivery {
        panic!("these cases use a native engine");
    }

    fn hosted_unread(&self, _engine: &EngineId) -> Option<usize> {
        None
    }
}

fn key() -> KeyRef {
    KeyRef::Wayland(crate::wayland::KeyRef {
        seat: 1,
        ctx: 1,
        keycode: 30,
        time: 1,
        released: false,
        repeat: false,
        modifiers: Default::default(),
    })
}

fn flight(method: &'static str, mode: Option<&str>) -> InFlight {
    InFlight {
        _task: RpcTask::default(),
        token: 1,
        engine: "rime".into(),
        method,
        deadline: Instant::now() + Duration::from_secs(1),
        key: (method == "ProcessKey").then(key),
        timed_out: false,
        app: Some("chat".into()),
        mode: mode.map(str::to_owned),
        mode_revision: 0,
        input_method: Some("rime/pinyin".into()),
        epoch: 0,
    }
}

fn mode_action(method: &str, mode: &str, revision: u64) -> WireAction {
    Action::InputMode {
        input_method: method.into(),
        mode: mode.into(),
        revision,
    }
    .to_wire()
}

fn mode_core() -> Core {
    let (events, _) = mpsc::unbounded_channel();
    let (ui, _) = mpsc::unbounded_channel();
    let (manifest, _) =
        Manifest::parse("[engine]\nid = 'rime'\nname = 'Rime'\nexec = ['false']\napi = '1.2'\n")
            .unwrap();
    let mut config = Config::default();
    config.apps.remember = true;
    config.apps.remember_mode = true;
    let mut core = Core::new(
        &config,
        vec![InstalledEngine {
            manifest,
            dir: "/tmp/unused-mode-test".into(),
            unknown_keys: Vec::new(),
        }],
        events,
        ui,
        None,
    );
    core.current = Some("rime".into());
    core.focused_app = Some("chat".into());
    let slot = core.engines.get_mut("rime").unwrap();
    slot.input_modes = vec!["native".into(), "latin".into()];
    slot.inner = Some("pinyin".into());
    slot.methods = vec![InnerMethod {
        id: "pinyin".into(),
        name: "Pinyin".into(),
        ..Default::default()
    }];
    core.contexts.insert(
        1,
        Context {
            origin: Origin::Fcitx,
            info: ContextInfo {
                app_id: Some("chat".into()),
                ..Default::default()
            },
            engine: Some("rime".into()),
            activated: true,
            queue: VecDeque::new(),
            in_flight: None,
            recovering: false,
            waiting_since: Instant::now(),
            hold_until: None,
            framework_ready: true,
            epoch: 0,
            mode_revision: 0,
            mode_ready: true,
            mode: None,
            pending_mode: None,
        },
    );
    core
}

#[tokio::test]
async fn settings_closed_without_a_running_process_keeps_pending_work() {
    for state in [
        EngineState::Stopped,
        EngineState::Stopping,
        EngineState::Failed,
    ] {
        let mut core = mode_core();
        let mut out = TestFrontends::default();
        core.engines.get_mut("rime").unwrap().state = state;
        let context = core.contexts.get_mut(&1).unwrap();
        context.in_flight = Some(flight("ProcessKey", None));
        context.mode = Some(("pinyin".into(), "latin".into()));
        context.queue.push_back(Request::Reset);
        core.on_event(
            CoreEvent::SettingsClosed {
                engine: "rime".into(),
            },
            &mut out,
        )
        .await;
        let context = &core.contexts[&1];
        assert!(context.in_flight.is_some());
        assert!(context.activated && context.mode_ready);
        assert_eq!(context.mode, Some(("pinyin".into(), "latin".into())));
        assert_eq!(context.queue.len(), 1);
        assert!(out.finished.is_empty());
        assert_eq!(core.engines.get("rime").unwrap().state, state);
    }
}

#[test]
fn mode_reports_require_the_live_application_method_and_capability() {
    let mut core = mode_core();
    core.accept_mode("rime", 1, "pinyin".into(), "latin".into(), 1);
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "latin");
    for (ctx, method, mode) in [
        (2, "pinyin", "native"),
        (1, "other", "native"),
        (1, "pinyin", "invalid"),
    ] {
        core.accept_mode("rime", ctx, method.into(), mode.into(), 2);
    }
    core.focused_app = Some("terminal".into());
    core.accept_mode("rime", 1, "pinyin".into(), "native".into(), 3);
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "latin");
    assert!(!core.app_modes.contains_key("terminal"));
    assert_eq!(core.current_status().unwrap().label.as_deref(), Some("A"));
    core.contexts.clear();
    core.accept_mode("rime", 1, "pinyin".into(), "native".into(), 4);
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "latin");
}

#[test]
fn activation_state_cannot_overwrite_a_mode_waiting_to_be_restored() {
    let mut core = mode_core();
    core.app_modes.insert(
        "chat".into(),
        BTreeMap::from([("rime/pinyin".into(), "native".into())]),
    );
    core.contexts.get_mut(&1).unwrap().mode_ready = false;
    core.accept_mode("rime", 1, "pinyin".into(), "latin".into(), 5);
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "native");
    assert_eq!(
        core.current_status().unwrap().input_mode.as_deref(),
        Some("latin")
    );
    core.contexts.get_mut(&1).unwrap().mode_ready = true;
    core.accept_mode("rime", 1, "pinyin".into(), "native".into(), 6);
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "native");
    core.forget_app("chat").unwrap();
    assert!(core.app_modes.is_empty());
}

#[test]
fn mode_memory_is_opt_in_but_status_is_always_confirmed() {
    let mut core = mode_core();
    core.remember_mode = false;
    core.accept_mode("rime", 1, "pinyin".into(), "latin".into(), 7);
    assert!(core.app_modes.is_empty());
    assert_eq!(
        core.current_status().unwrap().input_mode.as_deref(),
        Some("latin")
    );
}
#[test]
fn an_older_key_reply_cannot_undo_a_newer_menu_mode() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    core.contexts.get_mut(&1).unwrap().in_flight = Some(flight("ProcessKey", None));
    core.on_host_call(
        "rime",
        HostCall::InputMode {
            ctx: 1,
            input_method: "pinyin".into(),
            mode: "latin".into(),
            revision: 12,
        },
        &mut out,
    );
    core.on_reply(
        1,
        1,
        Reply::Key(Ok((true, vec![mode_action("pinyin", "native", 11)]))),
        &mut out,
    );
    assert_eq!(
        core.current_status().unwrap().input_mode.as_deref(),
        Some("latin")
    );
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "latin");
    assert_eq!(core.contexts[&1].mode_revision, 12);
    assert_eq!(out.finished, [(key(), true)]);
    assert!(
        out.actions.is_empty(),
        "mode confirmations are not text actions"
    );
}

#[test]
fn restoration_rejection_preserves_the_saved_mode_and_notifies_status() {
    for reply in [
        Reply::Actions(Ok(Vec::new())),
        Reply::Actions(Ok(vec![mode_action("other", "native", 2)])),
        Reply::Actions(Err(zbus::Error::Failure("engine refused".into()))),
    ] {
        let mut core = mode_core();
        let mut out = TestFrontends::default();
        core.accept_mode("rime", 1, "pinyin".into(), "native".into(), 1);
        let context = core.contexts.get_mut(&1).unwrap();
        context.mode_ready = false;
        context.in_flight = Some(flight("SetInputMode", Some("native")));
        // A new engine session really starts in Latin, before restoring native.
        core.accept_mode("rime", 1, "pinyin".into(), "latin".into(), 2);
        let changed = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = changed.clone();
        core.on_status = Some(Box::new(move |_, _, _| count.set(count.get() + 1)));
        core.on_reply(1, 1, reply, &mut out);
        assert_eq!(core.app_modes["chat"]["rime/pinyin"], "native");
        assert_eq!(
            core.current_status().unwrap().input_mode.as_deref(),
            Some("latin")
        );
        assert!(!core.contexts[&1].mode_ready);
        assert!(core.input_mode_error().is_some());
        assert_eq!(
            changed.get(),
            1,
            "the settings must learn of a restoration failure"
        );
        assert!(out.actions.is_empty());
    }
}

#[test]
fn a_restore_cannot_confirm_an_observation_from_before_the_request() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    core.accept_mode("rime", 1, "pinyin".into(), "native".into(), 1);
    core.contexts.get_mut(&1).unwrap().mode_ready = false;
    core.accept_mode("rime", 1, "pinyin".into(), "latin".into(), 5);
    let mut pending = flight("SetInputMode", Some("native"));
    pending.mode_revision = 5;
    core.contexts.get_mut(&1).unwrap().in_flight = Some(pending);
    core.on_reply(
        1,
        1,
        Reply::Actions(Ok(vec![mode_action("pinyin", "native", 4)])),
        &mut out,
    );
    assert!(!core.contexts[&1].mode_ready);
    assert!(core.input_mode_error().is_some());
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "native");
    assert_eq!(
        core.current_status().unwrap().input_mode.as_deref(),
        Some("latin")
    );
}

#[test]
fn deployment_can_confirm_a_pending_restore_and_clear_its_error() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    core.accept_mode("rime", 1, "pinyin".into(), "latin".into(), 1);
    let context = core.contexts.get_mut(&1).unwrap();
    context.mode_ready = false;
    context.pending_mode = Some(("rime/pinyin".into(), "latin".into()));
    context.in_flight = Some(flight("SetInputMode", Some("latin")));
    core.accept_mode("rime", 1, "pinyin".into(), "native".into(), 2);
    core.on_reply(1, 1, Reply::Actions(Ok(Vec::new())), &mut out);
    assert!(core.input_mode_error().is_some());
    core.on_host_call(
        "rime",
        HostCall::InputMode {
            ctx: 1,
            input_method: "pinyin".into(),
            mode: "latin".into(),
            revision: 3,
        },
        &mut out,
    );
    assert!(core.contexts[&1].mode_ready);
    assert!(core.input_mode_error().is_none());
    assert_eq!(
        core.current_status().unwrap().input_mode.as_deref(),
        Some("latin")
    );
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "latin");
}

#[tokio::test]
async fn reports_from_a_previous_engine_connection_are_ignored() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    let slot = core.engines.get_mut("rime").unwrap();
    slot.state = EngineState::Ready;
    slot.connection_id = Some(2);
    core.accept_mode("rime", 1, "pinyin".into(), "native".into(), 1);
    core.on_event(
        CoreEvent::Host {
            admission: None,
            engine: "rime".into(),
            connection_id: 1,
            call: HostCall::InputMode {
                ctx: 1,
                input_method: "pinyin".into(),
                mode: "latin".into(),
                revision: 99,
            },
        },
        &mut out,
    )
    .await;
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "native");
    core.on_event(
        CoreEvent::Host {
            admission: None,
            engine: "rime".into(),
            connection_id: 2,
            call: HostCall::InputMode {
                ctx: 1,
                input_method: "pinyin".into(),
                mode: "latin".into(),
                revision: 2,
            },
        },
        &mut out,
    )
    .await;
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "latin");
}

#[test]
fn a_newer_menu_mode_during_restoration_is_saved_after_confirmation() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    core.accept_mode("rime", 1, "pinyin".into(), "native".into(), 1);
    let context = core.contexts.get_mut(&1).unwrap();
    context.mode_ready = false;
    context.in_flight = Some(flight("SetInputMode", Some("native")));
    core.on_host_call(
        "rime",
        HostCall::InputMode {
            ctx: 1,
            input_method: "pinyin".into(),
            mode: "latin".into(),
            revision: 12,
        },
        &mut out,
    );
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "native");
    core.on_reply(
        1,
        1,
        Reply::Actions(Ok(vec![mode_action("pinyin", "native", 11)])),
        &mut out,
    );
    assert!(core.contexts[&1].mode_ready);
    assert_eq!(core.contexts[&1].mode_revision, 12);
    assert_eq!(
        core.current_status().unwrap().input_mode.as_deref(),
        Some("latin")
    );
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "latin");
}

#[test]
fn identifying_the_initial_app_preserves_its_pending_activation_mode() {
    for saved_mode in [None, Some("latin")] {
        let mut core = mode_core();
        let mut out = TestFrontends::default();
        core.engines.get_mut("rime").unwrap().state = EngineState::Ready;
        if let Some(mode) = saved_mode {
            core.app_modes.insert(
                "chat".into(),
                BTreeMap::from([("rime/pinyin".into(), mode.into())]),
            );
        }
        let context = core.contexts.get_mut(&1).unwrap();
        context.info.app_id = None;
        context.mode_ready = false;
        let mut activation = flight("Activate", None);
        activation.app = None;
        context.in_flight = Some(activation);
        context.queue.push_back(Request::RestoreMode);

        // text-input-v3 can focus a field before foreign-toplevel identifies its app.
        core.on_frontend_event(
            FrontendEvent::Updated {
                ctx: 1,
                info: ContextInfo {
                    app_id: Some("chat".into()),
                    ..Default::default()
                },
                reset: false,
            },
            &mut out,
        );
        core.on_reply(
            1,
            1,
            Reply::Actions(Ok(vec![mode_action("pinyin", "native", 1)])),
            &mut out,
        );
        assert_eq!(
            core.current_status().unwrap().input_mode.as_deref(),
            Some("native"),
            "the live field's initial mode is known before any key"
        );
        assert_eq!(
            core.app_modes["chat"]["rime/pinyin"],
            saved_mode.unwrap_or("native"),
            "activation cannot replace a saved mode awaiting restoration"
        );
    }
}

#[test]
fn identifying_an_app_does_not_relabel_pending_keys_or_later_app_changes() {
    for (method, apps) in [
        ("ProcessKey", vec!["chat"]),
        ("Activate", vec!["chat", "terminal", "chat"]),
    ] {
        let mut core = mode_core();
        let mut out = TestFrontends::default();
        core.accept_mode("rime", 1, "pinyin".into(), "native".into(), 1);
        let context = core.contexts.get_mut(&1).unwrap();
        context.info.app_id = None;
        let mut pending = flight(method, None);
        pending.app = None;
        context.in_flight = Some(pending);
        for app in apps {
            core.on_frontend_event(
                FrontendEvent::Updated {
                    ctx: 1,
                    info: ContextInfo {
                        app_id: Some(app.into()),
                        ..Default::default()
                    },
                    reset: false,
                },
                &mut out,
            );
        }
        core.on_reply(
            1,
            1,
            Reply::Actions(Ok(vec![mode_action("pinyin", "latin", 2)])),
            &mut out,
        );
        assert_eq!(
            core.current_status().unwrap().input_mode.as_deref(),
            Some("native")
        );
        assert_eq!(core.app_modes["chat"]["rime/pinyin"], "native");
    }
}

#[test]
fn a_reply_from_before_an_app_round_trip_cannot_change_its_mode() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    core.accept_mode("rime", 1, "pinyin".into(), "native".into(), 1);
    core.contexts.get_mut(&1).unwrap().in_flight = Some(flight("ProcessKey", None));
    for app in ["terminal", "chat"] {
        core.on_frontend_event(
            FrontendEvent::Updated {
                ctx: 1,
                info: ContextInfo {
                    app_id: Some(app.into()),
                    ..Default::default()
                },
                reset: false,
            },
            &mut out,
        );
    }
    assert_eq!(core.contexts[&1].epoch, 2);
    core.on_reply(
        1,
        1,
        Reply::Key(Ok((false, vec![mode_action("pinyin", "latin", 2)]))),
        &mut out,
    );
    assert_eq!(
        core.current_status().unwrap().input_mode.as_deref(),
        Some("native")
    );
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "native");
    assert_eq!(core.contexts[&1].mode_revision, 1);
    assert_eq!(out.finished, [(key(), false)]);
}

#[test]
fn choosing_a_schema_during_startup_keeps_one_activation_before_restoration() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    let slot = core.engines.get_mut("rime").unwrap();
    slot.state = EngineState::Starting {
        deadline: Instant::now() + Duration::from_secs(1),
    };
    slot.methods.push(InnerMethod {
        id: "shuangpin".into(),
        ..Default::default()
    });
    let context = core.contexts.get_mut(&1).unwrap();
    context.activated = false;
    context.queue.push_back(Request::Activate);
    context.queue.push_back(Request::RestoreMode);
    for method in ["shuangpin", "pinyin", "shuangpin"] {
        core.set_current(Some(format!("rime/{method}")), &mut out)
            .unwrap();
        let requests: Vec<_> = core.contexts[&1]
            .queue
            .iter()
            .map(Request::method)
            .collect();
        assert_eq!(requests, ["Activate", "SetInputMode"]);
    }
    assert!(out.finished.is_empty());
}

#[test]
fn schema_selection_confirmation_releases_waiting_mode_restoration() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    let slot = core.engines.get_mut("rime").unwrap();
    slot.state = EngineState::Ready;
    slot.methods.push(InnerMethod {
        id: "shuangpin".into(),
        ..Default::default()
    });
    slot.wanted_inner = Some("shuangpin".into());
    let context = core.contexts.get_mut(&1).unwrap();
    context.mode_ready = false;
    context.queue.push_back(Request::RestoreMode);
    core.pump(1, &mut out);
    assert!(matches!(
        core.contexts[&1].queue.front(),
        Some(Request::RestoreMode)
    ));
    assert!(!core.contexts[&1].mode_ready);
    core.on_host_call(
        "rime",
        HostCall::SetCurrentInputMethod("shuangpin".into()),
        &mut out,
    );
    assert!(core.contexts[&1].queue.is_empty());
    assert!(
        core.contexts[&1].mode_ready,
        "a method without saved mode can now take keys"
    );
    assert!(core.engines.get("rime").unwrap().wanted_inner.is_none());
}

#[test]
fn chooser_rejects_disabled_methods_and_a_different_focused_application() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    assert!(
        core.choose_input_method("rime/missing".into(), "chat".into(), &mut out)
            .is_err()
    );
    assert!(
        core.choose_input_method(String::new(), "terminal".into(), &mut out)
            .is_err()
    );
    assert_eq!(core.current(), Some("rime"));
    core.focused_app = None;
    core.contexts.clear();
    core.choose_input_method(String::new(), "chat".into(), &mut out)
        .unwrap();
    assert_eq!(core.current(), None);
    assert_eq!(core.app_engines.get("chat"), Some(&None));
}

fn numbered_key(number: u32) -> (KeyRef, KeyEvent) {
    let KeyRef::Wayland(mut reference) = key() else {
        unreachable!()
    };
    reference.keycode += number;
    reference.time = number;
    reference.released = number % 2 == 1;
    reference.modifiers.depressed = number;
    (
        KeyRef::Wayland(reference),
        KeyEvent {
            keysym: u32::from(b'a') + number,
            keycode: reference.keycode,
            modifiers: number,
            time: number,
            text: "a".into(),
            released: reference.released,
            repeat: false,
        },
    )
}

fn update_value(name: &str, value: impl Into<zbus::Variant<'static>>) -> Dict {
    Dict::from([(
        name.to_owned(),
        zbus::OwnedVariant::try_from(value.into()).unwrap(),
    )])
}

#[test]
fn adjacent_updates_coalesce_without_crossing_key_reset_or_restore() {
    let mut core = mode_core();
    let context = core.contexts.get_mut(&1).unwrap();
    let (key, event) = numbered_key(2);
    context.update(update_value("surrounding-text", "before"));
    context.update(update_value("surrounding-cursor", 1_u32));
    context.update(update_value("surrounding-text", "at-key"));
    context.queue.push_back(Request::Key(key, event, None));
    context.update(update_value("surrounding-text", "after-key"));
    context.queue.push_back(Request::Reset);
    context.update(update_value("surrounding-text", "after-reset"));
    context.queue.push_back(Request::RestoreMode);
    // Explicit clear markers are ordinary last-value fields too (including new
    // fields added by later protocol revisions).
    context.update(update_value("surrounding-available", true));
    context.update(update_value("surrounding-available", false));
    assert_eq!(
        context
            .queue
            .iter()
            .map(Request::method)
            .collect::<Vec<_>>(),
        [
            "UpdateContext",
            "ProcessKey",
            "UpdateContext",
            "Reset",
            "UpdateContext",
            "SetInputMode",
            "UpdateContext",
        ]
    );
    for (index, text) in [(0, "at-key"), (2, "after-key"), (4, "after-reset")] {
        let Request::Update(changes) = &context.queue[index] else {
            panic!("update expected")
        };
        assert_eq!(
            <&str>::try_from(&changes["surrounding-text"]).unwrap(),
            text
        );
    }
    let Request::Update(first) = &context.queue[0] else {
        unreachable!()
    };
    assert_eq!(u32::try_from(&first["surrounding-cursor"]).unwrap(), 1);
    let Request::Update(last) = &context.queue[6] else {
        unreachable!()
    };
    assert!(!bool::try_from(&last["surrounding-available"]).unwrap());
}

#[test]
fn queue_saturation_completes_keys_once_in_snapshot_order_and_stays_bounded() {
    for payload_limit in [false, true] {
        let mut core = mode_core();
        let mut out = TestFrontends::default();
        core.contexts.get_mut(&1).unwrap().in_flight = Some(flight("ProcessKey", None));
        let mut expected = vec![(key(), false)];
        for n in 1..=3 {
            let (key, event) = numbered_key(n);
            expected.push((key, false));
            core.on_frontend_event(FrontendEvent::Key { key, event }, &mut out);
        }
        assert!(out.finished.is_empty());
        if payload_limit {
            // A single overlarge key payload takes the same safe path as a full
            // update queue; its reference (including modifiers/release) survives.
            let (key, mut event) = numbered_key(4);
            event.text = "x".repeat(MAX_QUEUED_BYTES + 1);
            expected.push((key, false));
            core.on_frontend_event(FrontendEvent::Key { key, event }, &mut out);
        } else {
            // Resets prevent adjacent-update coalescing, exercising the count cap.
            for n in 0..MAX_QUEUED_REQUESTS {
                core.on_frontend_event(
                    FrontendEvent::Updated {
                        ctx: 1,
                        info: ContextInfo {
                            app_id: Some("chat".into()),
                            surrounding: Some((n.to_string(), 0, 0)),
                            ..Default::default()
                        },
                        reset: true,
                    },
                    &mut out,
                );
                assert!(core.contexts[&1].queue.len() <= MAX_QUEUED_REQUESTS);
                assert!(core.contexts[&1].queued_payload_bytes() <= MAX_QUEUED_BYTES);
            }
        }
        assert_eq!(out.finished, expected);
        let hard_deadline = core.contexts[&1].in_flight.as_ref().unwrap().deadline;
        assert!(core.contexts[&1].in_flight.as_ref().unwrap().timed_out);
        // Further arrivals cannot extend hard recovery or hold a later key.
        for n in 5..25 {
            let (key, event) = numbered_key(n);
            expected.push((key, false));
            core.on_frontend_event(FrontendEvent::Key { key, event }, &mut out);
            assert_eq!(out.finished, expected);
        }
        assert_eq!(
            core.contexts[&1].in_flight.as_ref().unwrap().deadline,
            hard_deadline
        );
        core.engines.get_mut("rime").unwrap().state = EngineState::Starting {
            deadline: hard_deadline + Duration::from_secs(1),
        };
        core.on_reply(
            1,
            1,
            Reply::Key(Ok((
                true,
                vec![Action::Commit("duplicate".into()).to_wire()],
            ))),
            &mut out,
        );
        assert!(
            out.actions.is_empty(),
            "the abandoned reply cannot duplicate passed keys"
        );
        assert_eq!(out.finished, expected);
        assert!(matches!(
            core.contexts[&1].queue.front(),
            Some(Request::Reset)
        ));
        // Recovery remains a bypass even while its Reset is in flight.
        core.contexts.get_mut(&1).unwrap().in_flight = Some(flight("Reset", None));
        let (key, event) = numbered_key(26);
        core.on_frontend_event(FrontendEvent::Key { key, event }, &mut out);
        expected.push((key, false));
        assert_eq!(out.finished, expected);
    }
}

#[test]
fn queued_update_payload_is_bounded_even_with_fewer_than_64_requests() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    core.contexts.get_mut(&1).unwrap().in_flight = Some(flight("UpdateContext", None));
    let mut triggered = false;
    for n in 0..32 {
        core.on_frontend_event(
            FrontendEvent::Updated {
                ctx: 1,
                info: ContextInfo {
                    app_id: Some("chat".into()),
                    surrounding: Some((format!("{n:04}{}", "x".repeat(3996)), 0, 0)),
                    ..Default::default()
                },
                reset: true,
            },
            &mut out,
        );
        let context = &core.contexts[&1];
        assert!(context.queued_payload_bytes() <= MAX_QUEUED_BYTES);
        if context.recovering {
            assert!(
                n < MAX_QUEUED_REQUESTS / 2,
                "payload cap must trigger before count cap"
            );
            triggered = true;
            break;
        }
    }
    assert!(triggered);
}

#[test]
fn queued_key_arrival_deadline_covers_healthy_update_backlogs() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    let mut pending = flight("UpdateContext", None);
    pending.deadline = Instant::now() + Duration::from_secs(5);
    core.contexts.get_mut(&1).unwrap().in_flight = Some(pending);
    let (key, event) = numbered_key(1);
    core.on_frontend_event(FrontendEvent::Key { key, event }, &mut out);
    let arrival_deadline = core.contexts[&1].queued_key_deadline().unwrap();
    assert_eq!(core.deadline(), Some(arrival_deadline));
    for n in 0..256 {
        core.on_frontend_event(
            FrontendEvent::Updated {
                ctx: 1,
                info: ContextInfo {
                    app_id: Some("chat".into()),
                    surrounding: Some(("ab".into(), n % 2, n % 2)),
                    ..Default::default()
                },
                reset: false,
            },
            &mut out,
        );
    }
    assert_eq!(
        core.contexts[&1].queued_key_deadline(),
        Some(arrival_deadline)
    );
    core.on_timer(arrival_deadline, &mut out);
    assert_eq!(out.finished, [(key, false)]);
    assert!(core.contexts[&1].in_flight.as_ref().unwrap().timed_out);
}

struct DroppedTask(std::rc::Rc<std::cell::Cell<usize>>);
impl Drop for DroppedTask {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

fn pending_task(dropped: &std::rc::Rc<std::cell::Cell<usize>>) -> RpcTask {
    let sentinel = DroppedTask(dropped.clone());
    RpcTask::new(tokio::task::spawn_local(async move {
        let _sentinel = sentinel;
        std::future::pending::<()>().await;
    }))
}

async fn wait_dropped(dropped: &std::rc::Rc<std::cell::Cell<usize>>, expected: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while dropped.get() < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled tasks must be dropped when the runtime polls them");
    assert_eq!(dropped.get(), expected);
}

#[tokio::test]
async fn every_retirement_path_drops_owned_rpc_tasks() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for route in [
                "deactivate",
                "switch",
                "unavailable",
                "disconnect",
                "hard-timeout",
                "drop",
            ] {
                let mut core = mode_core();
                let mut out = TestFrontends::default();
                let dropped = std::rc::Rc::new(std::cell::Cell::new(0));
                let mut pending = flight("ProcessKey", None);
                pending._task = pending_task(&dropped);
                let deadline = pending.deadline;
                if route == "hard-timeout" {
                    pending.timed_out = true;
                }
                core.contexts.get_mut(&1).unwrap().in_flight = Some(pending);
                let (queued_key, event) = numbered_key(2);
                core.contexts
                    .get_mut(&1)
                    .unwrap()
                    .queue
                    .push_back(Request::Key(queued_key, event, None));
                match route {
                    "deactivate" => {
                        core.on_frontend_event(FrontendEvent::Deactivated { ctx: 1 }, &mut out)
                    }
                    "switch" => core.set_current(None, &mut out).unwrap(),
                    "unavailable" => core.engine_unavailable("rime", &mut out),
                    "disconnect" => {
                        core.contexts.get_mut(&1).unwrap().origin = Origin::Wayland(1);
                        core.forget_wayland_contexts();
                    }
                    "hard-timeout" => core.on_timer(deadline, &mut out),
                    "drop" => {}
                    _ => unreachable!(),
                }
                if !matches!(route, "disconnect" | "drop") {
                    assert_eq!(
                        out.finished,
                        [(key(), false), (queued_key, false)],
                        "{route}"
                    );
                    core.on_reply(
                        1,
                        1,
                        Reply::Key(Ok((true, vec![Action::Commit("late".into()).to_wire()]))),
                        &mut out,
                    );
                    assert!(out.actions.is_empty(), "{route}");
                }
                if route != "drop" {
                    wait_dropped(&dropped, 1).await;
                }
                drop(core);
                wait_dropped(&dropped, 1).await;
            }
        })
        .await;
}

#[tokio::test]
async fn soft_timeout_keeps_rpc_until_reply_then_orders_reset() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut core = mode_core();
            let mut out = TestFrontends::default();
            let dropped = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut pending = flight("ProcessKey", None);
            pending._task = pending_task(&dropped);
            let deadline = pending.deadline;
            core.contexts.get_mut(&1).unwrap().in_flight = Some(pending);
            core.on_timer(deadline, &mut out);
            tokio::task::yield_now().await;
            assert_eq!(
                dropped.get(),
                0,
                "soft timeouts must preserve request/reset order"
            );
            assert_eq!(out.finished, [(key(), false)]);
            core.engines.get_mut("rime").unwrap().state = EngineState::Starting {
                deadline: deadline + Duration::from_secs(5),
            };
            core.on_reply(
                1,
                1,
                Reply::Key(Ok((true, vec![Action::Commit("late".into()).to_wire()]))),
                &mut out,
            );
            tokio::task::yield_now().await;
            assert_eq!(dropped.get(), 1);
            assert!(out.actions.is_empty());
            assert_eq!(out.finished, [(key(), false)]);
            assert!(matches!(
                core.contexts[&1].queue.front(),
                Some(Request::Reset)
            ));
        })
        .await;
}

#[tokio::test]
async fn residual_deactivation_tasks_are_bounded_expire_and_drop() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for expiry in [false, true] {
                let mut core = mode_core();
                let mut out = TestFrontends::default();
                let dropped = std::rc::Rc::new(std::cell::Cell::new(0));
                let deadline = Instant::now() + Duration::from_secs(1);
                for _ in 0..MAX_DEACTIVATIONS {
                    assert!(core.admit_deactivation("rime"));
                    core.deactivations.push(Deactivation {
                        engine: "rime".into(),
                        task: pending_task(&dropped),
                        deadline,
                    });
                }
                if expiry {
                    assert_eq!(core.deadline(), Some(deadline));
                    core.on_timer(deadline, &mut out);
                } else {
                    assert!(!core.admit_deactivation("rime"));
                }
                wait_dropped(&dropped, MAX_DEACTIVATIONS).await;
                assert!(core.deactivations.is_empty());
            }
        })
        .await;
}

#[test]
fn a_retired_token_cannot_take_the_new_context_request() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    let mut current = flight("ProcessKey", None);
    current.token = 2;
    core.contexts.get_mut(&1).unwrap().in_flight = Some(current);
    core.on_reply(
        1,
        1,
        Reply::Key(Ok((true, vec![Action::Commit("stale".into()).to_wire()]))),
        &mut out,
    );
    assert!(out.actions.is_empty() && out.finished.is_empty());
    assert_eq!(core.contexts[&1].in_flight.as_ref().unwrap().token, 2);
    core.on_reply(1, 2, Reply::Key(Ok((false, Vec::new()))), &mut out);
    assert_eq!(out.finished, [(key(), false)]);
}

#[test]
fn continuation_review_recovery_preserves_saved_mode_until_restore() {
    let mut core = mode_core();
    let mut out = TestFrontends::default();
    core.accept_mode("rime", 1, "pinyin".into(), "latin".into(), 1);
    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "latin");
    core.contexts.get_mut(&1).unwrap().in_flight = Some(flight("ProcessKey", None));
    let (key, mut event) = numbered_key(2);
    event.text = "x".repeat(MAX_QUEUED_BYTES + 1);
    core.on_frontend_event(FrontendEvent::Key { key, event }, &mut out);
    assert!(core.contexts[&1].recovering);
    // Hold dispatch so this unit test can inject the next ordered RPC reply.
    core.engines.get_mut("rime").unwrap().state = EngineState::Starting {
        deadline: Instant::now() + Duration::from_secs(5),
    };
    core.on_reply(1, 1, Reply::Key(Ok((false, Vec::new()))), &mut out);
    assert!(matches!(
        core.contexts[&1].queue.front(),
        Some(Request::Reset)
    ));
    assert!(
        core.contexts[&1]
            .queue
            .iter()
            .any(|r| matches!(r, Request::RestoreMode))
    );
    let context = core.contexts.get_mut(&1).unwrap();
    context.queue.pop_front();
    let mut reset = flight("Reset", None);
    reset.token = 2;
    context.in_flight = Some(reset);
    core.on_reply(
        1,
        2,
        Reply::Actions(Ok(vec![mode_action("pinyin", "native", 2)])),
        &mut out,
    );
    assert_eq!(
        core.app_modes["chat"]["rime/pinyin"], "latin",
        "Reset must not replace the saved mode before queued RestoreMode reads it"
    );
}

#[tokio::test]
async fn recovery_restores_saved_mode_before_composition_and_keeps_newer_observations() {
    tokio::task::LocalSet::new()
        .run_until(async {
            // A private peer supplies the proxy for the real send/pump path. The actor
            // replies below are injected synchronously, before local RPC tasks poll.
            let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
            let (client, peer) = tokio::join!(
                zbus::connection::Builder::unix_stream(client).p2p().build(),
                zbus::connection::Builder::unix_stream(server)
                    .server(zbus::Guid::generate())
                    .p2p()
                    .build(),
            );
            let client = client.unwrap();
            let _peer = peer.unwrap();
            for newer_observation in [false, true] {
                let mut core = mode_core();
                let mut out = TestFrontends::default();
                let slot = core.engines.get_mut("rime").unwrap();
                slot.state = EngineState::Ready;
                slot.proxy = Some(
                    engines::EngineProxy::builder(&client)
                        .build()
                        .await
                        .unwrap(),
                );
                core.accept_mode("rime", 1, "pinyin".into(), "latin".into(), 1);
                let context = core.contexts.get_mut(&1).unwrap();
                // An earlier failed restore may still be awaiting an asynchronous
                // confirmation. Reset's native observation must not complete it.
                context.pending_mode = Some(("rime/pinyin".into(), "native".into()));
                context.in_flight = Some(flight("ProcessKey", None));
                core.next_token = 1;
                let (overflow_key, mut event) = numbered_key(2);
                event.text = "x".repeat(MAX_QUEUED_BYTES + 1);
                core.on_frontend_event(
                    FrontendEvent::Key {
                        key: overflow_key,
                        event,
                    },
                    &mut out,
                );
                assert!(core.contexts[&1].recovering);
                assert!(!core.contexts[&1].mode_ready);
                assert!(core.contexts[&1].pending_mode.is_none());
                let mut completed = vec![(key(), false), (overflow_key, false)];
                assert_eq!(out.finished, completed);
                core.on_reply(1, 1, Reply::Key(Ok((true, Vec::new()))), &mut out);

                for (stage, revision) in [("Reset", 2), ("UpdateContext", 3)] {
                    let pending = core.contexts[&1].in_flight.as_ref().unwrap();
                    assert_eq!(pending.method, stage);
                    let token = pending.token;
                    let (bypass_key, event) = numbered_key(revision);
                    core.on_frontend_event(
                        FrontendEvent::Key {
                            key: bypass_key,
                            event,
                        },
                        &mut out,
                    );
                    completed.push((bypass_key, false));
                    assert_eq!(out.finished, completed, "keys bypass during {stage}");
                    assert_eq!(core.contexts[&1].in_flight.as_ref().unwrap().token, token);
                    core.on_reply(
                        1,
                        token,
                        Reply::Actions(Ok(vec![mode_action("pinyin", "native", revision.into())])),
                        &mut out,
                    );
                    assert_eq!(
                        core.current_status().unwrap().input_mode.as_deref(),
                        Some("native")
                    );
                    assert_eq!(
                        core.app_modes["chat"]["rime/pinyin"], "latin",
                        "{stage} cannot change the restore target"
                    );
                    assert!(!core.contexts[&1].mode_ready);
                }

                let restore = core.contexts[&1].in_flight.as_ref().unwrap();
                assert_eq!(restore.method, "SetInputMode");
                assert_eq!(
                    restore.mode.as_deref(),
                    Some("latin"),
                    "the real send path must restore the original target"
                );
                let token = restore.token;
                let (bypass_key, event) = numbered_key(4);
                core.on_frontend_event(
                    FrontendEvent::Key {
                        key: bypass_key,
                        event,
                    },
                    &mut out,
                );
                completed.push((bypass_key, false));
                assert_eq!(out.finished, completed);
                assert_eq!(core.contexts[&1].in_flight.as_ref().unwrap().token, token);
                if newer_observation {
                    core.on_host_call(
                        "rime",
                        HostCall::InputMode {
                            ctx: 1,
                            input_method: "pinyin".into(),
                            mode: "native".into(),
                            revision: 5,
                        },
                        &mut out,
                    );
                    assert_eq!(core.app_modes["chat"]["rime/pinyin"], "latin");
                }
                core.on_reply(
                    1,
                    token,
                    Reply::Actions(Ok(vec![mode_action("pinyin", "latin", 4)])),
                    &mut out,
                );
                let expected = if newer_observation { "native" } else { "latin" };
                assert_eq!(core.app_modes["chat"]["rime/pinyin"], expected);
                assert_eq!(
                    core.current_status().unwrap().input_mode.as_deref(),
                    Some(expected)
                );
                assert!(core.contexts[&1].mode_ready);
                assert!(!core.contexts[&1].recovering);
                assert!(core.contexts[&1].pending_mode.is_none());

                // Only a key after the ordered restoration can start composition.
                let (resumed_key, event) = numbered_key(6);
                core.on_frontend_event(
                    FrontendEvent::Key {
                        key: resumed_key,
                        event,
                    },
                    &mut out,
                );
                assert_eq!(out.finished, completed);
                let pending = core.contexts[&1].in_flight.as_ref().unwrap();
                assert_eq!(pending.method, "ProcessKey");
                assert_eq!(pending.key, Some(resumed_key));
                let token = pending.token;
                core.on_reply(1, token, Reply::Key(Ok((true, Vec::new()))), &mut out);
                completed.push((resumed_key, true));
                assert_eq!(out.finished, completed);
            }
        })
        .await;
}
