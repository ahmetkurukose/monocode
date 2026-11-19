//! What the user defines: a named command line with its working directory
//! and environment, and the ordered list of them a project keeps. They are
//! defined once per project and resolved per checkout, so one definition
//! works in every agent worktree.
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
        self
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
        Some(self.configs.remove(index))
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
