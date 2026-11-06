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

    fn apply_actions(&mut self, _origin: Origin, _ctx: ContextId, actions: &[Action]) {
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
