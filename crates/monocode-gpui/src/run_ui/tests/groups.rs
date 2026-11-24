//! Run groups through a real shell: members started in order with a delay and
//! a port wait, the three answers to a failed start, a member that ends later,
//! members that run already, Stop all, the footer's rows and the settings
//! page. The processes are short `sh -c` scripts.
use super::{footer::shape, *};
use monocode_core::code::run::{
    GroupMember, OnFailedStart, OnMemberExit, RunEvent, RunGroup, StartOrder, Wait,
};
use std::net::TcpListener;

const GROUP: &str = "Dev environment";

fn free_port() -> u16 {
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    probe.local_addr().unwrap().port()
}

fn member(config: &str, label: &str, wait: Wait) -> GroupMember {
    GroupMember {
        config: config.into(),
        label: label.into(),
        wait,
        ..GroupMember::default()
    }
}

impl Fixture {
    /// Saves `group` with the project's runs and returns its id.
    fn define(&mut self, group: RunGroup) -> String {
        let project = self.project.clone();
        let id = self.feature(move |window, cx| {
            let mut runs = settings::runs(&project, cx);
            let id = runs.add_group(group);
            settings::save_runs(&project, runs, window, cx);
            id
        });
        self.visual.run_until_parked();
        id
    }

    /// Three configurations that run until stopped, and a group of them in
    /// list order. `tune` changes the group before it is saved.
    fn dev_group(&mut self, tune: impl FnOnce(&mut RunGroup)) -> String {
        let ids = self.configure(&[
            ("Tunnel", "sh -c 'echo tunnel; sleep 30'"),
            ("Backend", "sh -c 'echo backend; sleep 30'"),
            ("Frontend", "sh -c 'echo frontend; sleep 30'"),
        ]);
        let mut group = RunGroup::named(GROUP);
        for (id, name) in ids.iter().zip(["Tunnel", "Backend", "Frontend"]) {
            group.add_member(id, name);
        }
        tune(&mut group);
        self.define(group)
    }

    fn press_group(&mut self, id: &str) {
        self.pick(&format!("group-go:{id}"));
    }

    /// As `wait`, and time moves on: delays and port checks happen on the
    /// executor's clock, which a test has to push.
    fn wait_ticking(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        for _ in 0..1500 {
            self.visual.run_until_parked();
            if done(self) {
                return;
            }
            let step = Duration::from_millis(300);
            self.visual.cx.executor().advance_clock(step);
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    /// Lets real time pass until `done`, which sees the fixture.
    fn wait_for(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        for _ in 0..1500 {
            self.visual.run_until_parked();
            if done(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    /// Lets the executor settle for a moment of real time, for something that
    /// must not happen.
    fn settle(&mut self) {
        for _ in 0..20 {
            self.visual.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn labels(&self) -> Vec<String> {
        self.runs().into_iter().map(|info| info.label).collect()
    }

    fn running_labels(&self) -> Vec<String> {
        (self.runs().into_iter())
            .filter(|info| info.state.is_running())
            .map(|info| info.label)
            .collect()
    }

    fn state_of(&self, label: &str) -> RunState {
        let info = self.runs().into_iter().find(|info| info.label == label);
        info.unwrap_or_else(|| panic!("no run {label}")).state
    }

    fn notice(&mut self) -> String {
        self.visual
            .update(|_, cx| start::last_notice(cx).unwrap_or_default())
    }

    /// Ends whatever of this project still runs, so no test leaves a process.
    fn end_all(&mut self) {
        for info in self.runs() {
            services().runs().stop(info.id);
        }
        self.wait_for("every run to end", |fx| {
            fx.runs().iter().all(|info| !info.state.is_running())
        });
    }
}

#[gpui::test]
fn a_sequential_group_starts_in_order_waiting_a_delay_and_then_a_port(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let port = free_port();
    let group = fx.dev_group(|group| {
        group.members[0].wait = Wait::Seconds(1);
        group.members[1].wait = Wait::Port(port);
    });
    let events = services().runs().subscribe();
    fx.press_group(&group);
    fx.started(1);
    assert_eq!(fx.labels(), ["Dev environment: Tunnel"]);
    // A second press while it starts is refused, not a second sequence.
    fx.press_group(&group);
    fx.settle();
    assert_eq!(fx.runs().len(), 1);
    assert_eq!(
        fx.notice(),
        "\u{201c}Dev environment\u{201d} is still starting."
    );
    // The footer says what the group waits for, as one row.
    assert_eq!(
        shape(&fx.menu())[..2],
        [
            "[Running]",
            "Dev environment \u{00b7} waiting for Tunnel > Stop all, Stop Tunnel"
        ]
    );
    fx.settle();
    assert_eq!(fx.labels().len(), 1, "the delay has not passed");

    fx.wait_ticking("the second member", |fx| fx.runs().len() == 2);
    assert_eq!(fx.labels()[1], "Dev environment: Backend");
    for _ in 0..8 {
        fx.visual.run_until_parked();
        fx.visual
            .cx
            .executor()
            .advance_clock(Duration::from_millis(300));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fx.runs().len(), 2, "the port is not open yet");
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("the port is free");
    fx.wait_ticking("the third member", |fx| fx.runs().len() == 3);
    assert_eq!(
        fx.labels(),
        [
            "Dev environment: Tunnel",
            "Dev environment: Backend",
            "Dev environment: Frontend"
        ]
    );
    assert_eq!(fx.tabs(), 3);
    assert_eq!(fx.running_labels().len(), 3);

    // The menu lists the group as one row, and the members only inside it.
    let menu = shape(&fx.menu());
    assert_eq!(
        menu[..2],
        [
            "[Running]",
            "Dev environment \u{00b7} 3 of 3 running > Stop all, Stop Tunnel, Stop Backend, Stop Frontend"
        ]
    );
    assert_eq!(
        menu[2..],
        [
            "[Groups]",
            "Dev environment",
            "[Run configurations]",
            "Tunnel",
            "Backend",
            "Frontend"
        ]
    );
    let project = fx.project.clone();
    assert_eq!(
        fx.visual
            .update(|_, cx| crate::run_ui::footer::running_count(&project, cx)),
        3
    );

    // Stop all ends the members last first.
    fx.click("footer-action-menu");
    fx.click(Box::leak(
        format!("menu-action-group-running:{group}").into_boxed_str(),
    ));
    fx.click(Box::leak(
        format!("menu-action-group-stop:{group}").into_boxed_str(),
    ));
    fx.wait_for("every member to stop", |fx| fx.running_labels().is_empty());
    let asked: Vec<String> = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            RunEvent::StopRequested(id) => services().runs().get(id).map(|info| info.label),
            _ => None,
        })
        .collect();
    assert_eq!(
        asked,
        [
            "Dev environment: Frontend",
            "Dev environment: Backend",
            "Dev environment: Tunnel"
        ]
    );
    for label in fx.labels() {
        assert_eq!(fx.state_of(&label), RunState::Stopped, "{label}");
    }
    assert!(
        !shape(&fx.menu())
            .iter()
            .any(|row| row.starts_with("Dev environment \u{00b7}"))
    );
    drop(listener);
    fx.end_all();
}

#[gpui::test]
fn a_member_that_ends_before_its_wait_leaves_the_first_running_and_the_third_unstarted(
    cx: &mut TestAppContext,
) {
    let mut fx = Fixture::new(&[], cx);
    let ids = fx.configure(&[
        ("Tunnel", "sh -c 'sleep 30'"),
        ("Backend", "sh -c 'exit 3'"),
        ("Frontend", "sh -c 'sleep 30'"),
    ]);
    let mut spec = RunGroup::named("Dev");
    spec.members = vec![
        member(&ids[0], "Tunnel", Wait::None),
        member(&ids[1], "Backend", Wait::Seconds(30)),
        member(&ids[2], "Frontend", Wait::None),
    ];
    let group = fx.define(spec);
    fx.press_group(&group);
    fx.wait_for("the failure", |fx| {
        fx.runs()
            .iter()
            .any(|info| info.state == RunState::Exited(Some(3)))
    });
    fx.settle();
    assert_eq!(
        fx.labels(),
        ["Dev: Tunnel", "Dev: Backend"],
        "the third never starts"
    );
    assert_eq!(fx.running_labels(), ["Dev: Tunnel"]);
    assert_eq!(
        fx.notice(),
        "\u{201c}Dev\u{201d} did not start the rest. \u{201c}Backend\u{201d} exited with code 3 before it was ready."
    );
    fx.end_all();
}

#[gpui::test]
fn the_same_failure_stops_the_first_member_when_the_group_says_so(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let ids = fx.configure(&[
        ("Tunnel", "sh -c 'sleep 30'"),
        ("Backend", "sh -c 'exit 3'"),
    ]);
    let mut spec = RunGroup::named("Dev");
    spec.on_failed_start = OnFailedStart::StopGroup;
    spec.members = vec![
        member(&ids[0], "Tunnel", Wait::None),
        member(&ids[1], "Backend", Wait::Seconds(30)),
    ];
    let group = fx.define(spec);
    fx.press_group(&group);
    fx.wait_for("the first member to be stopped", |fx| {
        fx.runs()
            .iter()
            .any(|info| info.label == "Dev: Tunnel" && info.state == RunState::Stopped)
    });
    assert!(fx.running_labels().is_empty());
    assert!(
        fx.notice().starts_with("\u{201c}Dev\u{201d} was stopped."),
        "{}",
        fx.notice()
    );
    fx.end_all();
}

#[gpui::test]
fn a_failed_start_that_is_skipped_goes_on_with_the_next_member(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    // The second member needs a file, which nothing has focused.
    let ids = fx.configure(&[
        ("Tunnel", "sh -c 'sleep 30'"),
        ("Needs a file", "sh -c 'echo $FILE_NAME'"),
        ("Frontend", "sh -c 'sleep 30'"),
    ]);
    let mut spec = RunGroup::named("Dev");
    spec.on_failed_start = OnFailedStart::Continue;
    spec.members = vec![
        member(&ids[0], "Tunnel", Wait::None),
        member(&ids[1], "Needs a file", Wait::None),
        member(&ids[2], "Frontend", Wait::None),
    ];
    let group = fx.define(spec);
    fx.press_group(&group);
    fx.started(2);
    fx.settle();
    assert_eq!(fx.labels(), ["Dev: Tunnel", "Dev: Frontend"]);
    assert!(fx.notice().contains("Needs a file"), "{}", fx.notice());
    fx.end_all();
}

#[gpui::test]
fn a_member_that_ends_later_stops_the_group_or_leaves_the_others(cx: &mut TestAppContext) {
    for stop in [true, false] {
        let mut fx = Fixture::new(&[], cx);
        let ids = fx.configure(&[
            ("Tunnel", "sh -c 'sleep 30'"),
            ("Backend", "sh -c 'sleep 0.3'"),
        ]);
        let mut spec = RunGroup::named("Dev");
        spec.on_member_exit = match stop {
            true => OnMemberExit::StopGroup,
            false => OnMemberExit::LeaveOthers,
        };
        spec.members = vec![
            member(&ids[0], "Tunnel", Wait::None),
            member(&ids[1], "Backend", Wait::None),
        ];
        let group = fx.define(spec);
        fx.press_group(&group);
        fx.wait_for("the second member to end", |fx| {
            fx.runs()
                .iter()
                .any(|info| info.label == "Dev: Backend" && !info.state.is_running())
        });
        match stop {
            true => {
                fx.wait_for("the first member to be stopped", |fx| {
                    fx.running_labels().is_empty()
                });
                assert_eq!(fx.state_of("Dev: Tunnel"), RunState::Stopped);
                assert!(fx.notice().contains("Backend"), "{}", fx.notice());
            }
            false => {
                fx.settle();
                assert_eq!(fx.running_labels(), ["Dev: Tunnel"]);
            }
        }
        fx.end_all();
    }
}

#[gpui::test]
fn a_member_stopped_by_hand_never_stops_the_group(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let group = fx.dev_group(|group| {
        group.on_member_exit = OnMemberExit::StopGroup;
        group.on_failed_start = OnFailedStart::StopGroup;
    });
    fx.press_group(&group);
    fx.started(3);
    fx.wait_for("the members to run", |fx| fx.running_labels().len() == 3);
    // The member's own Stop row, inside the group's, as the user picks it.
    let backend = fx.runs()[1].id;
    fx.click("footer-action-menu");
    fx.click(Box::leak(
        format!("menu-action-group-running:{group}").into_boxed_str(),
    ));
    fx.click(Box::leak(
        format!("menu-action-stop:{}", backend.0).into_boxed_str(),
    ));
    fx.wait_for("the member to stop", |fx| {
        fx.state_of("Dev environment: Backend") == RunState::Stopped
    });
    fx.settle();
    assert_eq!(
        fx.running_labels(),
        ["Dev environment: Tunnel", "Dev environment: Frontend"]
    );
    fx.end_all();
}

#[gpui::test]
fn pressing_a_group_again_starts_only_what_is_missing(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let port = free_port();
    let _open = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let ids = fx.configure(&[
        ("Tunnel", "sh -c 'echo tunnel; sleep 30'"),
        ("Backend", "sh -c 'echo backend; sleep 30'"),
    ]);
    let mut spec = RunGroup::named("Dev");
    spec.members = vec![
        member(&ids[0], "Tunnel", Wait::Port(port)),
        member(&ids[1], "Backend", Wait::None),
    ];
    let group = fx.define(spec);
    // The first member runs already, on its own.
    fx.pick(&format!("go:{}", ids[0]));
    fx.started(1);
    let tunnel = fx.runs()[0].id;
    fx.wait("the tunnel to run", |_| {
        services()
            .runs()
            .get(tunnel)
            .is_some_and(|i| i.state.is_running())
    });

    fx.press_group(&group);
    fx.wait_ticking("the backend", |fx| fx.runs().len() == 2);
    fx.settle();
    assert_eq!(
        fx.labels(),
        ["Tunnel", "Dev: Backend"],
        "the tunnel was not restarted"
    );
    assert!(services().runs().get(tunnel).unwrap().state.is_running());

    // With everything running, a press starts nothing.
    fx.press_group(&group);
    fx.settle();
    assert_eq!(fx.runs().len(), 2);
    assert_eq!(fx.running_labels().len(), 2);
    fx.end_all();
}

#[gpui::test]
fn a_parallel_group_starts_everything_at_once(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let group = fx.dev_group(|group| {
        group.order = StartOrder::Parallel;
        // Ignored in a parallel group.
        group.members[0].wait = Wait::Port(1);
    });
    fx.press_group(&group);
    fx.wait_for("all three runs", |fx| fx.running_labels().len() == 3);
    let mut labels = fx.labels();
    labels.sort();
    assert_eq!(
        labels,
        [
            "Dev environment: Backend",
            "Dev environment: Frontend",
            "Dev environment: Tunnel"
        ]
    );
    fx.end_all();
}

#[gpui::test]
fn a_member_whose_configuration_is_gone_is_skipped_and_a_group_of_gone_members_says_so(
    cx: &mut TestAppContext,
) {
    let mut fx = Fixture::new(&[], cx);
    let ids = fx.configure(&[("Tunnel", "sh -c 'sleep 30'")]);
    let mut spec = RunGroup::named("Dev");
    spec.members = vec![
        member("gone", "Backend", Wait::None),
        member(&ids[0], "Tunnel", Wait::None),
    ];
    let group = fx.define(spec);
    fx.press_group(&group);
    fx.started(1);
    assert_eq!(fx.labels(), ["Dev: Tunnel"]);
    assert!(
        fx.notice().contains("\u{201c}Backend\u{201d} is skipped"),
        "{}",
        fx.notice()
    );
    fx.end_all();

    let mut all_gone = RunGroup::named("Lost");
    all_gone.members = vec![member("gone", "Backend", Wait::None)];
    let lost = fx.define(all_gone);
    fx.press_group(&lost);
    fx.settle();
    assert_eq!(
        fx.notice(),
        "\u{201c}Lost\u{201d} cannot start: none of its run configurations exist any more."
    );
}

#[gpui::test]
fn quick_open_has_a_run_and_a_stop_command_for_a_group(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let group = fx.dev_group(|_| {});
    let scope = Scope {
        project: fx.project.clone(),
        folder: fx.project.clone(),
    };
    let titles = |fx: &mut Fixture| -> Vec<String> {
        fx.visual.update(|_, cx| {
            (commands::commands(&scope, cx).iter())
                .map(|command| command.title.to_string())
                .filter(|title| title.contains(GROUP))
                .collect()
        })
    };
    assert_eq!(titles(&mut fx), ["Run: Dev environment"]);
    let command = fx.visual.update(|_, cx| {
        (commands::commands(&scope, cx).into_iter())
            .find(|command| command.title == "Run: Dev environment")
            .unwrap()
    });
    fx.feature(|window, cx| (command.run)(window, cx));
    fx.wait_for("every member", |fx| fx.running_labels().len() == 3);
    assert_eq!(
        titles(&mut fx),
        ["Run: Dev environment", "Run: Stop Dev environment"]
    );
    let stop = fx.visual.update(|_, cx| {
        (commands::commands(&scope, cx).into_iter())
            .find(|command| command.title == "Run: Stop Dev environment")
            .unwrap()
    });
    fx.feature(|window, cx| (stop.run)(window, cx));
    fx.wait_for("every member to stop", |fx| fx.running_labels().is_empty());
    let _ = group;
    fx.end_all();
}

#[gpui::test]
fn the_page_edits_groups_and_saves_them_with_the_runs(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let project = fx.project.clone();
    let ids = fx.configure(&[("Tunnel", "ssh -N db"), ("Backend", "npm start")]);
    let page = {
        let project = project.clone();
        fx.feature(move |_, cx| cx.new(|cx| RunsPage::new(project, None, cx)))
    };
    type Edit = Box<dyn FnOnce(&mut RunsPage, &mut Window, &mut gpui::Context<RunsPage>)>;
    let edit = |fx: &mut Fixture, f: Edit| {
        let page = page.clone();
        fx.feature(move |window, cx| page.update(cx, |page, cx| f(page, window, cx)));
        fx.visual.run_until_parked();
    };
    let saved = |fx: &mut Fixture| {
        let project = fx.project.clone();
        fx.window
            .update(&mut fx.visual, |shell, _, _| {
                shell.workspace.project_runs[&project].clone()
            })
            .unwrap()
    };
    assert!(saved(&mut fx).groups.is_empty());

    // A new group is saved at once, with the defaults.
    edit(
        &mut fx,
        Box::new(|page, window, cx| page.add_group(window, cx)),
    );
    let runs = saved(&mut fx);
    assert_eq!(runs.groups.len(), 1);
    let group = &runs.groups[0];
    assert_eq!(group.name, "New group");
    assert_eq!(
        (group.order, group.on_failed_start, group.on_member_exit),
        (
            StartOrder::Sequential,
            OnFailedStart::DoNotStartRest,
            OnMemberExit::LeaveOthers
        )
    );

    let (tunnel, backend) = (ids[0].clone(), ids[1].clone());
    edit(
        &mut fx,
        Box::new(move |page, window, cx| {
            page.add_member(&tunnel, window, cx);
            page.add_member(&backend, window, cx);
            page.set_wait_kind(0, WaitKind::Port, window, cx);
            page.set_member_field(0, MemberField::Value, "5432", cx);
            page.set_member_field(0, MemberField::Timeout, "20", cx);
            page.set_wait_kind(1, WaitKind::Seconds, window, cx);
            page.set_member_field(1, MemberField::Value, "7", cx);
            page.set_group_name("Dev stack", cx);
            page.set_order(StartOrder::Parallel, window, cx);
            page.set_on_failed_start(OnFailedStart::Continue, window, cx);
            page.set_on_member_exit(OnMemberExit::StopGroup, window, cx);
            page.shift_member(1, false, window, cx);
        }),
    );
    let runs = saved(&mut fx);
    let group = &runs.groups[0];
    assert_eq!(group.name, "Dev stack");
    assert_eq!(
        (group.order, group.on_failed_start, group.on_member_exit),
        (
            StartOrder::Parallel,
            OnFailedStart::Continue,
            OnMemberExit::StopGroup
        )
    );
    let members: Vec<(&str, Wait, u32)> = (group.members.iter())
        .map(|member| (member.label.as_str(), member.wait, member.timeout))
        .collect();
    assert_eq!(
        members,
        [
            ("Backend", Wait::Seconds(7), 60),
            ("Tunnel", Wait::Port(5432), 20)
        ]
    );

    // A number that is not one yet changes nothing; the list actions copy,
    // move and remove, and deleting a configuration takes it out of the group.
    edit(
        &mut fx,
        Box::new(|page, window, cx| {
            page.set_member_field(1, MemberField::Value, "70000", cx);
            assert_eq!(
                page.selected_group().unwrap().members[1].wait,
                Wait::Port(5432)
            );
            page.set_member_field(1, MemberField::Value, "5433", cx);
            page.save(window, cx);
            page.duplicate_group(window, cx);
            assert_eq!(page.selected_group().unwrap().name, "Dev stack copy");
            page.shift_group(false, window, cx);
        }),
    );
    let runs = saved(&mut fx);
    assert_eq!(
        runs.groups
            .iter()
            .map(|g| g.name.as_str())
            .collect::<Vec<_>>(),
        ["Dev stack copy", "Dev stack"]
    );
    assert_eq!(runs.groups[1].members[1].wait, Wait::Port(5433));
    edit(
        &mut fx,
        Box::new(|page, window, cx| {
            let tunnel = page.runs().configs[0].id.clone();
            page.select(Some(tunnel), cx);
            page.remove(window, cx);
        }),
    );
    let runs = saved(&mut fx);
    assert_eq!(runs.configs.len(), 1);
    for group in &runs.groups {
        assert_eq!(group.members.len(), 1);
        assert_eq!(group.members[0].label, "Backend");
    }
    edit(
        &mut fx,
        Box::new(|page, window, cx| page.remove_group(window, cx)),
    );
    assert_eq!(saved(&mut fx).groups.len(), 1);

    // What is saved is what a restart restores.
    let json = (fx.window)
        .update(&mut fx.visual, |shell, _, _| shell.workspace.saved_json())
        .unwrap();
    let restored = Workspace::restore(Some(&json), &[], &project);
    assert_eq!(restored.project_runs[&project], saved(&mut fx));
    assert!(json.contains("\"onMemberExit\":\"stopGroup\""), "{json}");
}

#[gpui::test]
fn the_settings_page_shows_the_groups_section(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    fx.dev_group(|_| {});
    let project = fx.project.clone();
    assert!(fx.feature(move |window, cx| settings::open(&project, PAGE, window, cx)));
    fx.visual.run_until_parked();
    assert!(fx.visual.debug_bounds("run-groups").is_some());
}
