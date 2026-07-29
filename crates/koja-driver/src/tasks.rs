//! Custom CLI task resolution and harness synthesis for
//! `koja run <task.name>`.
//!
//! Packages export tasks in their `koja.toml` `[tasks]` table, mapping
//! a package-prefixed task name (`postgres.migrate`) to a type
//! implementing `Koja.Task`. The driver collects the project's tasks
//! plus every dependency's, then the toolchain's own (the stdlib
//! `koja` package, e.g. `koja.new`), synthesizes a Process-shaped
//! harness that calls the task type's `run`, and lowers it as the
//! program entry.

use std::collections::BTreeMap;
use std::path::Path;

use koja_ast::util::dedent;

use crate::deps;
use crate::project::ProjectConfig;

/// Name of the synthesized task-harness entry type, spliced into the
/// providing package's namespace when lowering a task run.
pub(crate) const TASK_HARNESS_ENTRY: &str = "KojaTaskHarness";

/// One resolved task, recording which package exports it and the type
/// that runs it.
pub(crate) struct TaskProvider {
    /// The provider's code namespace, where the harness is spliced so
    /// the task type resolves unqualified.
    pub namespace: String,
    /// Lowercase package name, shown in listings and collision errors.
    pub package: String,
    /// Toolchain tasks ship with the stdlib `koja` package. They
    /// compile against a stdlib-only bundle (no project sources) and
    /// run even where no `koja.toml` exists.
    pub toolchain: bool,
    /// The PascalCase type implementing `Koja.Task`.
    pub type_name: String,
}

/// Collect every task in scope: the project's own `[tasks]` plus each
/// dependency's (when a project is loaded), then the toolchain's,
/// sorted by task name. The per-manifest prefix rule makes
/// cross-package collisions structurally impossible (`koja` itself is
/// a reserved package name), but any residual duplicate still errors,
/// naming both providers.
pub(crate) fn resolve_tasks(
    project: Option<(&ProjectConfig, &Path)>,
) -> Result<BTreeMap<String, TaskProvider>, String> {
    let mut tasks = BTreeMap::new();
    if let Some((config, root)) = project {
        insert_tasks(
            &mut tasks,
            config.tasks.iter().map(|(n, t)| (n.clone(), t.clone())),
            config.name.clone(),
            config.namespace(),
            false,
        )?;
        for dep in deps::sync_project(config, root)? {
            insert_tasks(
                &mut tasks,
                dep.tasks.iter().map(|(n, t)| (n.clone(), t.clone())),
                dep.name,
                dep.namespace,
                false,
            )?;
        }
    }
    insert_tasks(
        &mut tasks,
        koja_stdlib::TOOLCHAIN_TASKS
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string())),
        "koja".to_string(),
        "Koja".to_string(),
        true,
    )?;
    Ok(tasks)
}

fn insert_tasks(
    tasks: &mut BTreeMap<String, TaskProvider>,
    exported: impl Iterator<Item = (String, String)>,
    package: String,
    namespace: String,
    toolchain: bool,
) -> Result<(), String> {
    for (name, type_name) in exported {
        let provider = TaskProvider {
            namespace: namespace.clone(),
            package: package.clone(),
            toolchain,
            type_name,
        };
        if let Some(existing) = tasks.insert(name.clone(), provider) {
            return Err(format!(
                "task `{name}` is exported by both `{}` and `{package}`",
                existing.package
            ));
        }
    }
    Ok(())
}

/// Generate the Koja source for the task harness, a
/// [`TASK_HARNESS_ENTRY`] struct implementing
/// `Process<List<String>, (), ()>` (the argv shape, so both backends
/// feed it the command-line arguments) whose `run` calls the task
/// type's `run`. A `Result.Err` prints to stderr and exits 1 via
/// `StopReason.Shutdown`.
pub(crate) fn generate_task_harness(type_name: &str) -> String {
    dedent(&format!(
        r#"
        struct {TASK_HARNESS_ENTRY}
          args: List<String>
        end

        impl Process<List<String>, (), ()> for {TASK_HARNESS_ENTRY}
          fn start(config: List<String>) -> Result<Self, Process.StopReason>
            Result.Ok({TASK_HARNESS_ENTRY}{{args: config}})
          end

          fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Process.Step<Self>
            Process.Step.Continue(self)
          end

          fn run(self) -> Process.StopReason
            match {type_name}.run(self.args)
              Result.Ok(_) -> Process.StopReason.Normal
              Result.Err(message) ->
                IO.warn("error: " <> message)
                Process.StopReason.Shutdown
            end
          end
        end
        "#
    ))
}
