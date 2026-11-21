//! Run configurations (docs/native/code.md §3): what the user defines per
//! project, how a definition and the caret's context become one process to
//! start in a checkout, the tasks offered without defining anything, and
//! the registry of what is running. Headless: the terminal dock executes
//! the resolved `Spawn`, and the agents' MCP tools use the same calls.
mod config;
mod headless;
mod output;
mod ports;
mod registry;
mod resolve;
mod tasks;
mod variables;

pub use config::{ProjectRuns, RunConfig, TestRunner, BUILTIN_PREFIX};
pub use output::RunOutput;
pub use ports::{first_local_url, local_urls, machine_ips};
pub use registry::{registry, Registry, RunEvent, RunId, RunInfo, RunState, StartedBy};
pub use resolve::{resolve, variables_used, Unresolved};
pub use tasks::{
    builtin_tasks, is_python_test_file, runnable_tasks, task_languages, Runnable, RunnableKind,
};
pub use variables::{module_path, ModulePath, RunContext, Variable};

use crate::code::toolchain::Toolchain;

/// The project's configurations that can run in `context`, in the user's
/// order. One that needs something the context lacks (a focused file, a
/// selection) is left out rather than shown as broken.
pub fn offered<'a>(
    runs: &'a ProjectRuns,
    context: &RunContext,
    toolchain: &Toolchain,
) -> Vec<&'a RunConfig> {
    (runs.configs.iter())
        .filter(|config| resolve(config, context, toolchain).is_ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configuration_that_needs_a_file_is_offered_only_with_one() {
        let mut runs = ProjectRuns::default();
        for (name, command) in [
            ("Serve", "npm start"),
            ("Current file", "node $FILE"),
            ("Grep selection", "rg $SELECTED_TEXT"),
            ("Broken", "echo 'open"),
        ] {
            runs.add(RunConfig {
                name: name.into(),
                command: command.into(),
                ..RunConfig::default()
            });
        }
        let toolchain = Toolchain::default();
        let names = |context: &RunContext| -> Vec<String> {
            offered(&runs, context, &toolchain)
                .into_iter()
                .map(|config| config.name.clone())
                .collect()
        };
        let mut context = RunContext::new("/work/tree", "/work/tree");
        assert_eq!(names(&context), ["Serve"]);
        context.file = Some("/work/tree/a.js".into());
        assert_eq!(names(&context), ["Serve", "Current file"]);
        context.selected_text = Some("needle".into());
        assert_eq!(names(&context), ["Serve", "Current file", "Grep selection"]);
    }
}
