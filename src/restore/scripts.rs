//! Pre/post-restore hook scripts (feature `restore.pre-post-scripts`).
//!
//! The operator declares scripts to run at named stages of the restore. A
//! script is one of: a raw `mongosh` command, a path to a `mongosh` script
//! file, or an external command with arguments. A failing script stops the
//! restore. Execution is abstracted behind [`ScriptRunner`] so the staging and
//! failure semantics are unit-testable without spawning real processes.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::{Error, Result};

/// One declared script. Exactly one of `query`, `query_file`, or `command`
/// must be set.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScriptSpec {
    /// Optional human-readable name for logs.
    #[serde(default)]
    pub name: Option<String>,
    /// A raw mongosh command to evaluate.
    #[serde(default)]
    pub query: Option<String>,
    /// A path to a mongosh script file to run.
    #[serde(default)]
    pub query_file: Option<String>,
    /// An external command and its arguments.
    #[serde(default)]
    pub command: Option<Vec<String>>,
}

/// The resolved kind of a script after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptKind {
    Mongosh(String),
    MongoshFile(String),
    Command(Vec<String>),
}

impl ScriptSpec {
    /// Validate that exactly one driver is configured and return it.
    pub fn kind(&self) -> Result<ScriptKind> {
        let set = [
            self.query.is_some(),
            self.query_file.is_some(),
            self.command
                .as_ref()
                .map(|c| !c.is_empty())
                .unwrap_or(false),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        if set != 1 {
            return Err(Error::Restore(format!(
                "script '{}' must set exactly one of query, query_file, or command",
                self.name.as_deref().unwrap_or("<unnamed>")
            )));
        }
        if let Some(q) = &self.query {
            Ok(ScriptKind::Mongosh(q.clone()))
        } else if let Some(f) = &self.query_file {
            Ok(ScriptKind::MongoshFile(f.clone()))
        } else {
            Ok(ScriptKind::Command(self.command.clone().unwrap()))
        }
    }

    /// Run this script through `runner`.
    pub fn run(&self, runner: &dyn ScriptRunner) -> Result<()> {
        match self.kind()? {
            ScriptKind::Mongosh(cmd) => runner.run_mongosh(&cmd),
            ScriptKind::MongoshFile(path) => runner.run_mongosh_file(&path),
            ScriptKind::Command(argv) => runner.run_command(&argv),
        }
    }
}

/// Scripts grouped by restore stage (e.g. `pre-data`, `post-data`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Scripts {
    stages: BTreeMap<String, Vec<ScriptSpec>>,
}

impl Scripts {
    /// Run every script configured for `stage`, in order, stopping at the first
    /// failure so the restore does not proceed past a broken hook.
    pub fn run_stage(&self, stage: &str, runner: &dyn ScriptRunner) -> Result<()> {
        if let Some(scripts) = self.stages.get(stage) {
            for s in scripts {
                s.run(runner)?;
            }
        }
        Ok(())
    }

    /// The stages that have at least one script.
    pub fn stages(&self) -> impl Iterator<Item = &str> {
        self.stages.keys().map(String::as_str)
    }
}

/// Abstraction over how a script is actually executed.
pub trait ScriptRunner {
    fn run_mongosh(&self, command: &str) -> Result<()>;
    fn run_mongosh_file(&self, path: &str) -> Result<()>;
    fn run_command(&self, argv: &[String]) -> Result<()>;
}

/// The real runner: spawns `mongosh` (for commands/files) or the external
/// command directly, connecting to `mongo_uri`. A non-zero exit is an error.
pub struct ProcessScriptRunner {
    pub mongo_uri: String,
    pub mongosh_bin: String,
}

impl ProcessScriptRunner {
    pub fn new(mongo_uri: impl Into<String>) -> Self {
        ProcessScriptRunner {
            mongo_uri: mongo_uri.into(),
            mongosh_bin: "mongosh".to_string(),
        }
    }

    fn check(status: std::process::ExitStatus, what: &str) -> Result<()> {
        if status.success() {
            Ok(())
        } else {
            Err(Error::Restore(format!(
                "script {what} failed with {status}"
            )))
        }
    }
}

impl ScriptRunner for ProcessScriptRunner {
    fn run_mongosh(&self, command: &str) -> Result<()> {
        let status = std::process::Command::new(&self.mongosh_bin)
            .arg(&self.mongo_uri)
            .arg("--eval")
            .arg(command)
            .status()
            .map_err(|e| Error::Restore(format!("cannot start mongosh: {e}")))?;
        Self::check(status, "mongosh --eval")
    }

    fn run_mongosh_file(&self, path: &str) -> Result<()> {
        let status = std::process::Command::new(&self.mongosh_bin)
            .arg(&self.mongo_uri)
            .arg("--file")
            .arg(path)
            .status()
            .map_err(|e| Error::Restore(format!("cannot start mongosh: {e}")))?;
        Self::check(status, path)
    }

    fn run_command(&self, argv: &[String]) -> Result<()> {
        let (bin, args) = argv
            .split_first()
            .ok_or_else(|| Error::Restore("empty command".into()))?;
        let status = std::process::Command::new(bin)
            .args(args)
            .status()
            .map_err(|e| Error::Restore(format!("cannot start {bin}: {e}")))?;
        Self::check(status, bin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records what it was asked to run, and can be told to fail on a match.
    #[derive(Default)]
    struct RecordingRunner {
        calls: RefCell<Vec<String>>,
        fail_on: Option<String>,
    }
    impl ScriptRunner for RecordingRunner {
        fn run_mongosh(&self, command: &str) -> Result<()> {
            self.record(format!("mongosh:{command}"))
        }
        fn run_mongosh_file(&self, path: &str) -> Result<()> {
            self.record(format!("file:{path}"))
        }
        fn run_command(&self, argv: &[String]) -> Result<()> {
            self.record(format!("cmd:{}", argv.join(" ")))
        }
    }
    impl RecordingRunner {
        fn record(&self, entry: String) -> Result<()> {
            if let Some(f) = &self.fail_on {
                if entry.contains(f) {
                    return Err(Error::Restore(format!("boom on {entry}")));
                }
            }
            self.calls.borrow_mut().push(entry);
            Ok(())
        }
    }

    fn scripts(src: &str) -> Scripts {
        serde_yaml::from_str(src).unwrap()
    }

    // Acceptance: a configured script for a stage runs at that stage.
    #[test]
    fn runs_scripts_for_the_stage_in_order() {
        let s = scripts(
            "pre-data:\n  - query: db.users.drop()\n  - query: db.orders.drop()\npost-data:\n  - query: db.users.reIndex()\n",
        );
        let runner = RecordingRunner::default();
        s.run_stage("pre-data", &runner).unwrap();
        assert_eq!(
            *runner.calls.borrow(),
            vec!["mongosh:db.users.drop()", "mongosh:db.orders.drop()"]
        );
        // an unconfigured stage is a no-op.
        s.run_stage("nope", &runner).unwrap();
        assert_eq!(runner.calls.borrow().len(), 2);
    }

    // Acceptance: raw command, script file, and external command are all valid.
    #[test]
    fn supports_three_script_kinds() {
        assert_eq!(
            ScriptSpec {
                query: Some("db.x.drop()".into()),
                ..Default::default()
            }
            .kind()
            .unwrap(),
            ScriptKind::Mongosh("db.x.drop()".into())
        );
        assert_eq!(
            ScriptSpec {
                query_file: Some("/tmp/pre.js".into()),
                ..Default::default()
            }
            .kind()
            .unwrap(),
            ScriptKind::MongoshFile("/tmp/pre.js".into())
        );
        assert_eq!(
            ScriptSpec {
                command: Some(vec!["sh".into(), "-c".into(), "echo hi".into()]),
                ..Default::default()
            }
            .kind()
            .unwrap(),
            ScriptKind::Command(vec!["sh".into(), "-c".into(), "echo hi".into()])
        );
        // ambiguous / empty specs are rejected.
        assert!(ScriptSpec::default().kind().is_err());
        assert!(ScriptSpec {
            query: Some("a".into()),
            command: Some(vec!["b".into()]),
            ..Default::default()
        }
        .kind()
        .is_err());
    }

    // Acceptance: a failing script stops the restore and surfaces the error,
    // and later scripts do not run.
    #[test]
    fn failing_script_aborts_stage() {
        let s = scripts("pre-data:\n  - query: ok1\n  - command: [false]\n  - query: ok2\n");
        let runner = RecordingRunner {
            fail_on: Some("cmd:false".into()),
            ..Default::default()
        };
        let err = s.run_stage("pre-data", &runner).unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
        // only the first script ran; ok2 never executed.
        assert_eq!(*runner.calls.borrow(), vec!["mongosh:ok1"]);
    }
}
