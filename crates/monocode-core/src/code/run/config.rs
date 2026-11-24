//! What the user defines: a named command line with its working directory
//! and environment, and the ordered list of them a project keeps. They are
//! defined once per project and resolved per checkout, so one definition
//! works in every agent worktree.
use super::group::RunGroup;
use serde::{Deserialize, Serialize};

/// Ids of the tasks this app offers by itself start with this, so they can
/// never collide with a configuration the user made.
pub const BUILTIN_PREFIX: &str = "builtin:";

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct RunConfig {
    /// Stable for the life of the configuration; given by `ProjectRuns::add`.
    pub id: String,
    pub name: String,
    /// The command line as typed, split by shell rules when it is resolved.
    /// No shell runs it: pipes and `&&` need an explicit `sh -c '...'`.
    pub command: String,
    /// More arguments as typed, added after the command's own.
    pub args: String,
    /// Empty for the checkout, relative to the checkout, or absolute.
    pub cwd: String,
    /// `KEY=VALUE` lines as typed.
    pub env: String,
}

impl RunConfig {
    /// One of the tasks offered without the user defining anything.
    pub fn is_builtin(&self) -> bool {
        self.id.starts_with(BUILTIN_PREFIX)
    }

    /// What the run is called: the name, or the command line without one.
    pub fn label(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_owned();
        }
        [self.command.trim(), self.args.trim()]
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn normalized(mut self) -> Self {
        self.id = self.id.trim().to_owned();
        self.name = self.name.trim().to_owned();
        self.command = self.command.trim().to_owned();
        self.args = self.args.trim().to_owned();
        self.cwd = self.cwd.trim().to_owned();
        self.env = self.env.trim().to_owned();
        self
    }
}

/// What a project's Python tests are run with.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TestRunner {
    #[default]
    Pytest,
    Unittest,
}

/// A project's run settings, saved with the project's other settings.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectRuns {
    /// In the order the user arranged them. Change it through the methods,
    /// which keep the ids unique.
    pub configs: Vec<RunConfig>,
    /// What was run last, for the header's run button. A built-in task is
    /// kept whole, since nothing else remembers it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<RunConfig>,
    pub test_runner: TestRunner,
    /// Named lists of the configurations above that are started together.
    /// Absent from what is saved while there are none, so an older build
    /// that does not know them loses nothing it had.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<RunGroup>,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

impl ProjectRuns {
    /// Nothing differs from the defaults: the project needs no entry.
    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
            && self.last_used.is_none()
            && self.test_runner == TestRunner::Pytest
            && self.groups.is_empty()
    }

    /// As it should be saved: trimmed, without configurations that have no
    /// command, every configuration with an id of its own, and without a
    /// last-used entry that points at nothing.
    pub fn normalized(mut self) -> Self {
        let mut configs: Vec<RunConfig> = Vec::with_capacity(self.configs.len());
        for config in std::mem::take(&mut self.configs) {
            let mut config = config.normalized();
            if config.command.is_empty() {
                continue;
            }
            let taken = config.id.is_empty()
                || config.is_builtin()
                || configs.iter().any(|kept| kept.id == config.id);
            if taken {
                config.id = new_id();
            }
            configs.push(config);
        }
        self.configs = configs;
        self.last_used = self.last_used.take().map(RunConfig::normalized);
        if self.last_used().is_none() {
            self.last_used = None;
        }
        self.groups = self.normalized_groups();
        self
    }

    /// Groups without a name are left out like configurations without a
    /// command; every group gets an id of its own, and each member the name
    /// its configuration has now. A member whose configuration is gone is
    /// kept: the group names it when it is skipped.
    fn normalized_groups(&mut self) -> Vec<RunGroup> {
        let mut groups: Vec<RunGroup> = Vec::with_capacity(self.groups.len());
        for group in std::mem::take(&mut self.groups) {
            let mut group = group.normalized();
            if group.name.is_empty() {
                continue;
            }
            if group.id.is_empty() || groups.iter().any(|kept| kept.id == group.id) {
                group.id = new_id();
            }
            for member in &mut group.members {
                if let Some(config) = self.configs.iter().find(|c| c.id == member.config) {
                    member.label = config.label();
                }
            }
            groups.push(group);
        }
        groups
    }

    pub fn get(&self, id: &str) -> Option<&RunConfig> {
        self.configs.iter().find(|config| config.id == id)
    }

    /// Adds `config` at the end under a new id, which is returned.
    pub fn add(&mut self, mut config: RunConfig) -> String {
        config.id = new_id();
        let id = config.id.clone();
        self.configs.push(config);
        id
    }

    /// Replaces the configuration that has `config`'s id, keeping its
    /// place. False when there is none.
    pub fn update(&mut self, config: RunConfig) -> bool {
        match self.configs.iter_mut().find(|kept| kept.id == config.id) {
            Some(kept) => {
                *kept = config;
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<RunConfig> {
        let index = self.configs.iter().position(|config| config.id == id)?;
        if self.last_used.as_ref().is_some_and(|last| last.id == id) {
            self.last_used = None;
        }
        for group in &mut self.groups {
            group.members.retain(|member| member.config != id);
        }
        Some(self.configs.remove(index))
    }

    pub fn group(&self, id: &str) -> Option<&RunGroup> {
        self.groups.iter().find(|group| group.id == id)
    }

    pub fn group_mut(&mut self, id: &str) -> Option<&mut RunGroup> {
        self.groups.iter_mut().find(|group| group.id == id)
    }

    /// Adds `group` at the end under a new id, which is returned.
    pub fn add_group(&mut self, mut group: RunGroup) -> String {
        group.id = new_id();
        let id = group.id.clone();
        self.groups.push(group);
        id
    }

    pub fn remove_group(&mut self, id: &str) -> Option<RunGroup> {
        let index = self.groups.iter().position(|group| group.id == id)?;
        Some(self.groups.remove(index))
    }

    /// Moves a group so that it ends up at `index` (clamped to the list).
    /// False when there is no such group.
    pub fn move_group(&mut self, id: &str, index: usize) -> bool {
        let Some(from) = self.groups.iter().position(|group| group.id == id) else {
            return false;
        };
        let group = self.groups.remove(from);
        self.groups.insert(index.min(self.groups.len()), group);
        true
    }

    /// Moves a configuration so that it ends up at `index` (clamped to the
    /// list). False when there is no such configuration.
    pub fn move_to(&mut self, id: &str, index: usize) -> bool {
        let Some(from) = self.configs.iter().position(|config| config.id == id) else {
            return false;
        };
        let config = self.configs.remove(from);
        let index = index.min(self.configs.len());
        self.configs.insert(index, config);
        true
    }

    /// Remembers `config`, one of the project's own or a built-in task, as
    /// the one the run button repeats.
    pub fn mark_used(&mut self, config: &RunConfig) {
        self.last_used = Some(config.clone());
    }

    /// What the run button repeats: the configuration as it is now if it
    /// is one of the project's, the remembered task if it was built in.
    pub fn last_used(&self) -> Option<&RunConfig> {
        let last = self.last_used.as_ref()?;
        match last.is_builtin() {
            true => Some(last),
            false => self.get(&last.id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::run::group::{
        GroupMember, OnFailedStart, OnMemberExit, StartOrder, Wait, DEFAULT_WAIT_TIMEOUT,
    };

    fn config(name: &str, command: &str) -> RunConfig {
        RunConfig {
            name: name.into(),
            command: command.into(),
            ..RunConfig::default()
        }
    }

    fn names(runs: &ProjectRuns) -> Vec<&str> {
        runs.configs
            .iter()
            .map(|config| config.name.as_str())
            .collect()
    }

    #[test]
    fn configurations_round_trip_in_camel_case() {
        let mut runs = ProjectRuns::default();
        let id = runs.add(RunConfig {
            name: "Serve".into(),
            command: "python manage.py runserver".into(),
            args: "--noreload".into(),
            cwd: "web".into(),
            env: "DEBUG=1\nMODE=dev".into(),
            ..RunConfig::default()
        });
        runs.test_runner = TestRunner::Unittest;
        runs.mark_used(&runs.get(&id).unwrap().clone());
        let saved = serde_json::to_string(&runs).unwrap();
        assert!(saved.contains("\"lastUsed\""), "{saved}");
        assert!(saved.contains("\"testRunner\":\"unittest\""), "{saved}");
        assert_eq!(serde_json::from_str::<ProjectRuns>(&saved).unwrap(), runs);

        // Older or partial files load with defaults for what is absent.
        let partial: ProjectRuns =
            serde_json::from_str(r#"{"configs":[{"id":"a","command":"make"}]}"#).unwrap();
        assert_eq!(partial.configs[0].command, "make");
        assert_eq!(partial.configs[0].cwd, "");
        assert_eq!(partial.test_runner, TestRunner::Pytest);
        assert_eq!(partial.last_used, None);
        let plain = serde_json::to_string(&ProjectRuns::default()).unwrap();
        assert!(!plain.contains("lastUsed"));
        assert!(ProjectRuns::default().is_empty());
        assert!(!runs.is_empty());
    }

    #[test]
    fn ids_are_stable_through_add_update_remove_and_reorder() {
        let mut runs = ProjectRuns::default();
        let serve = runs.add(config("Serve", "npm start"));
        let test = runs.add(config("Test", "npm test"));
        let lint = runs.add(config("Lint", "npm run lint"));
        assert!(serve != test && test != lint && !serve.is_empty());
        assert_eq!(names(&runs), ["Serve", "Test", "Lint"]);

        // An id the caller supplies is not trusted to be unique.
        let copy = runs.add(runs.get(&serve).unwrap().clone());
        assert_ne!(copy, serve);
        assert!(runs.remove(&copy).is_some());

        let mut edited = runs.get(&test).unwrap().clone();
        edited.name = "Unit tests".into();
        assert!(runs.update(edited));
        assert_eq!(names(&runs), ["Serve", "Unit tests", "Lint"]);
        assert!(!runs.update(config("Ghost", "true")));

        let moves: [(&str, usize, [&str; 3]); 5] = [
            (&lint, 0, ["Lint", "Serve", "Unit tests"]),
            (&lint, 1, ["Serve", "Lint", "Unit tests"]),
            (&serve, 2, ["Lint", "Unit tests", "Serve"]),
            (&serve, 99, ["Lint", "Unit tests", "Serve"]),
            (&test, 1, ["Lint", "Unit tests", "Serve"]),
        ];
        for (id, index, expected) in moves {
            assert!(runs.move_to(id, index));
            assert_eq!(names(&runs), expected, "move to {index}");
        }
        assert!(!runs.move_to("absent", 0));
        assert_eq!(runs.get(&test).unwrap().name, "Unit tests");

        assert_eq!(runs.remove(&lint).unwrap().name, "Lint");
        assert!(runs.remove(&lint).is_none());
        assert_eq!(names(&runs), ["Unit tests", "Serve"]);
    }

    #[test]
    fn the_last_used_configuration_follows_edits_and_removal() {
        let mut runs = ProjectRuns::default();
        assert_eq!(runs.last_used(), None);
        let serve = runs.add(config("Serve", "npm start"));
        runs.mark_used(&runs.get(&serve).unwrap().clone());
        let mut edited = runs.get(&serve).unwrap().clone();
        edited.command = "npm run dev".into();
        runs.update(edited);
        assert_eq!(runs.last_used().unwrap().command, "npm run dev");

        // A built-in task is remembered whole.
        let task = RunConfig {
            id: format!("{BUILTIN_PREFIX}python:file:a.py"),
            name: "Run a.py".into(),
            command: "$PYTHON a.py".into(),
            ..RunConfig::default()
        };
        assert!(task.is_builtin());
        runs.mark_used(&task);
        assert_eq!(runs.last_used(), Some(&task));
        let saved = serde_json::to_string(&runs).unwrap();
        let restored = serde_json::from_str::<ProjectRuns>(&saved)
            .unwrap()
            .normalized();
        assert_eq!(restored.last_used(), Some(&task));

        runs.mark_used(&runs.get(&serve).unwrap().clone());
        runs.remove(&serve);
        assert_eq!(runs.last_used(), None);
        assert_eq!(runs.last_used, None);
    }

    #[test]
    fn saving_trims_and_repairs() {
        let runs = ProjectRuns {
            configs: vec![
                RunConfig {
                    id: "a".into(),
                    name: "  Serve ".into(),
                    command: " npm start\n".into(),
                    ..RunConfig::default()
                },
                config("No command", "   "),
                RunConfig {
                    id: "a".into(),
                    ..config("Twin", "npm test")
                },
                config("No id", "make"),
                RunConfig {
                    id: format!("{BUILTIN_PREFIX}x"),
                    ..config("Pretender", "true")
                },
            ],
            last_used: Some(RunConfig {
                id: "gone".into(),
                ..config("Gone", "true")
            }),
            test_runner: TestRunner::Pytest,
            groups: Vec::new(),
        }
        .normalized();
        assert_eq!(names(&runs), ["Serve", "Twin", "No id", "Pretender"]);
        assert_eq!(runs.configs[0].id, "a");
        assert_eq!(runs.configs[0].command, "npm start");
        let mut ids: Vec<&str> = runs
            .configs
            .iter()
            .map(|config| config.id.as_str())
            .collect();
        assert!(ids
            .iter()
            .all(|id| !id.is_empty() && !id.starts_with(BUILTIN_PREFIX)));
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4);
        assert_eq!(runs.last_used, None);
        assert!(ProjectRuns {
            configs: vec![config("", "")],
            ..ProjectRuns::default()
        }
        .normalized()
        .is_empty());
    }

    fn group_of(runs: &mut ProjectRuns, name: &str, configs: &[&str]) -> String {
        let mut group = RunGroup::named(name);
        for id in configs {
            let label = runs.get(id).map(RunConfig::label).unwrap_or_default();
            group.add_member(id, &label);
        }
        runs.add_group(group)
    }

    #[test]
    fn groups_round_trip_and_older_state_loads_without_them() {
        let mut runs = ProjectRuns::default();
        let tunnel = runs.add(config("Tunnel", "ssh -N db"));
        let backend = runs.add(config("Backend", "npm start"));
        let id = group_of(&mut runs, "Dev environment", &[&tunnel, &backend]);
        let group = runs.group_mut(&id).unwrap();
        group.order = StartOrder::Parallel;
        group.on_failed_start = OnFailedStart::StopGroup;
        group.on_member_exit = OnMemberExit::StopGroup;
        group.members[0].wait = Wait::Port(5432);
        group.members[0].timeout = 20;
        group.members[1].wait = Wait::Seconds(3);
        let saved = serde_json::to_string(&runs).unwrap();
        assert!(saved.contains("\"onFailedStart\":\"stopGroup\""), "{saved}");
        assert!(saved.contains("\"wait\":{\"port\":5432}"), "{saved}");
        assert_eq!(serde_json::from_str::<ProjectRuns>(&saved).unwrap(), runs);
        assert!(!runs.is_empty());
        assert!(!ProjectRuns {
            groups: runs.groups.clone(),
            ..ProjectRuns::default()
        }
        .is_empty());

        // State saved before groups existed, and one without settings, load
        // with the defaults.
        let old: ProjectRuns = serde_json::from_str(
            r#"{"configs":[{"id":"a","command":"make"}],"testRunner":"unittest"}"#,
        )
        .unwrap();
        assert!(old.groups.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("groups"));
        let bare: ProjectRuns = serde_json::from_str(
            r#"{"groups":[{"id":"g","name":"Dev","members":[{"config":"a"}]}]}"#,
        )
        .unwrap();
        let group = &bare.groups[0];
        assert_eq!(
            (group.order, group.on_failed_start, group.on_member_exit),
            (
                StartOrder::Sequential,
                OnFailedStart::DoNotStartRest,
                OnMemberExit::LeaveOthers
            )
        );
        assert_eq!(group.members[0].wait, Wait::None);
        assert_eq!(group.members[0].timeout, DEFAULT_WAIT_TIMEOUT);
    }

    #[test]
    fn deleting_a_configuration_removes_it_from_the_groups() {
        let mut runs = ProjectRuns::default();
        let a = runs.add(config("A", "a"));
        let b = runs.add(config("B", "b"));
        let first = group_of(&mut runs, "One", &[&a, &b]);
        let second = group_of(&mut runs, "Two", &[&b]);
        runs.remove(&b);
        let members = |runs: &ProjectRuns, id: &str| -> Vec<String> {
            (runs.group(id).unwrap().members.iter())
                .map(|member| member.label.clone())
                .collect()
        };
        assert_eq!(members(&runs, &first), ["A"]);
        assert!(members(&runs, &second).is_empty(), "the group stays, empty");
        assert!(runs.remove_group(&second).is_some());
        assert!(runs.remove_group(&second).is_none());
    }

    #[test]
    fn groups_are_repaired_and_follow_renamed_configurations() {
        let mut runs = ProjectRuns::default();
        let a = runs.add(config("A", "a"));
        let id = group_of(&mut runs, "  Dev ", &[&a]);
        runs.group_mut(&id).unwrap().members.push(GroupMember {
            config: "gone".into(),
            label: "Gone".into(),
            ..GroupMember::default()
        });
        runs.group_mut(&id).unwrap().members.push(GroupMember {
            config: a.clone(),
            ..GroupMember::default()
        });
        runs.groups.push(RunGroup {
            id: id.clone(),
            name: "Twin".into(),
            ..RunGroup::default()
        });
        runs.groups.push(RunGroup::named("   "));
        let mut renamed = runs.get(&a).unwrap().clone();
        renamed.name = "Alpha".into();
        runs.update(renamed);
        let runs = runs.normalized();
        assert_eq!(runs.groups.len(), 2, "the nameless group is dropped");
        assert_eq!(runs.groups[0].name, "Dev");
        assert_ne!(runs.groups[1].id, runs.groups[0].id);
        let members: Vec<(&str, &str)> = (runs.groups[0].members.iter())
            .map(|member| (member.config.as_str(), member.label.as_str()))
            .collect();
        // A duplicate is dropped; a member without a configuration keeps the
        // name it had, so it can be reported.
        assert_eq!(members, [(a.as_str(), "Alpha"), ("gone", "Gone")]);

        let mut group = runs.groups[0].clone();
        assert_eq!(group.problem(), None);
        group.members[0].wait = Wait::Port(0);
        assert!(group.problem().unwrap().contains("port 0"));
        group.name = " ".into();
        assert_eq!(group.problem().unwrap(), "A group needs a name.");
    }

    #[test]
    fn groups_move_and_their_members_move() {
        let mut runs = ProjectRuns::default();
        let one = group_of(&mut runs, "One", &[]);
        let two = group_of(&mut runs, "Two", &[]);
        let three = group_of(&mut runs, "Three", &[]);
        assert!(runs.move_group(&three, 0));
        let names = |runs: &ProjectRuns| -> Vec<String> {
            runs.groups.iter().map(|group| group.name.clone()).collect()
        };
        assert_eq!(names(&runs), ["Three", "One", "Two"]);
        assert!(runs.move_group(&three, 99));
        assert_eq!(names(&runs), ["One", "Two", "Three"]);
        assert!(!runs.move_group("absent", 0));
        let _ = (one, two);

        let mut group = RunGroup::named("G");
        assert!(group.add_member("a", "A") && group.add_member("b", "B"));
        assert!(!group.add_member("a", "A"));
        assert!(group.move_member(1, 0));
        assert_eq!(group.members[0].config, "b");
        assert!(!group.move_member(5, 0));
    }

    #[test]
    fn a_configuration_without_a_name_is_labelled_by_its_command() {
        let mut serve = config("", " npm start ");
        assert_eq!(serve.label(), "npm start");
        serve.args = "--port 1".into();
        assert_eq!(serve.label(), "npm start --port 1");
        serve.name = "Serve".into();
        assert_eq!(serve.label(), "Serve");
    }
}
