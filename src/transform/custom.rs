//! Custom/external transformers (feature `transform.custom-transformers`).
//!
//! The operator extends the engine with their own transformers, declared under
//! `custom_transformers`. Two drivers are supported:
//!   * `command` — spawns an executable that exchanges one record per line over
//!     stdin/stdout (the process is kept alive and streamed to);
//!   * `template` — a `{{ field }}` template (shares the built-in engine).
//! Custom transformers are registered into the same [`Registry`] as built-ins,
//! so they appear in the catalog and declare their own typed parameters.
//!
//! Gap: the source uses a BSON/length-framed protocol; here the command
//! protocol is one JSON value per line — see regeneration-gaps.md.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use bson::Bson;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::toolkit::{ParamDefinition, ParamType};

use super::{Registry, Transformer, TransformerFactory};

/// A declared parameter on a custom transformer.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CustomParamDef {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
}

impl CustomParamDef {
    fn to_definition(&self) -> ParamDefinition {
        let kind = match self.kind.as_str() {
            "int" => ParamType::Int,
            "float" => ParamType::Float,
            "bool" => ParamType::Bool,
            "objectId" => ParamType::ObjectId,
            "datetime" => ParamType::DateTime,
            _ => ParamType::String,
        };
        if self.required {
            ParamDefinition::required(&self.name, kind, "custom parameter")
        } else {
            ParamDefinition::optional(&self.name, kind, Bson::Null, "custom parameter")
        }
    }
}

/// A `custom_transformers` config entry.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CustomTransformerDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `command` or `template`.
    pub driver: String,
    /// Executable + args for the `command` driver.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Template text for the `template` driver.
    #[serde(default)]
    pub template: Option<String>,
    /// Declared typed parameters (for the catalog).
    #[serde(default)]
    pub parameters: Vec<CustomParamDef>,
}

/// Register every custom transformer definition into `r`, alongside built-ins.
pub fn register_custom(r: &mut Registry, defs: &[CustomTransformerDef]) -> Result<()> {
    for def in defs {
        let params: Vec<ParamDefinition> = def.parameters.iter().map(|p| p.to_definition()).collect();
        let description = def
            .description
            .clone()
            .unwrap_or_else(|| format!("custom {} transformer", def.driver));
        match def.driver.as_str() {
            "command" => {
                let argv = def
                    .command
                    .clone()
                    .filter(|c| !c.is_empty())
                    .ok_or_else(|| {
                        Error::Transform(format!("custom transformer '{}' needs a command", def.name))
                    })?;
                r.register(TransformerFactory::new(&def.name, &description, params, move |_, _| {
                    Ok(Box::new(CommandTransformer::spawn(&argv)?) as Box<dyn Transformer>)
                }));
            }
            "template" => {
                let template = def.template.clone().ok_or_else(|| {
                    Error::Transform(format!("custom transformer '{}' needs a template", def.name))
                })?;
                r.register(TransformerFactory::new(&def.name, &description, params, move |_, _| {
                    let template = template.clone();
                    let re = regex::Regex::new(r"\{\{\s*\.?(\w+)\s*\}\}").unwrap();
                    Ok(Box::new(super::FnTransformer(move |_v: &Bson, doc: &bson::Document| {
                        let out = re.replace_all(&template, |caps: &regex::Captures| {
                            match doc.get(&caps[1]) {
                                Some(Bson::String(s)) => s.clone(),
                                Some(b) => b.to_string(),
                                None => String::new(),
                            }
                        });
                        Ok(Bson::String(out.into_owned()))
                    })) as Box<dyn Transformer>)
                }));
            }
            other => {
                return Err(Error::Transform(format!(
                    "custom transformer '{}' has unknown driver '{other}' (command|template)",
                    def.name
                )));
            }
        }
    }
    Ok(())
}

/// A persistent external process exchanging one JSON value per line.
struct CommandTransformer {
    argv: Vec<String>,
    proc: Mutex<Proc>,
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl CommandTransformer {
    fn spawn(argv: &[String]) -> Result<Self> {
        let (bin, args) = argv
            .split_first()
            .ok_or_else(|| Error::Transform("empty custom command".into()))?;
        let mut child = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Transform(format!("cannot spawn '{bin}': {e}")))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Ok(CommandTransformer {
            argv: argv.to_vec(),
            proc: Mutex::new(Proc { child, stdin, stdout }),
        })
    }
}

impl Drop for CommandTransformer {
    fn drop(&mut self) {
        if let Ok(mut p) = self.proc.lock() {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }
}

impl Transformer for CommandTransformer {
    fn transform(&self, value: &Bson, _doc: &bson::Document) -> Result<Bson> {
        let mut p = self.proc.lock().unwrap();
        let json = bson_to_json(value);
        let line = serde_json::to_string(&json).map_err(|e| Error::Transform(e.to_string()))?;
        writeln!(p.stdin, "{line}").and_then(|_| p.stdin.flush()).map_err(|e| {
            Error::Transform(format!("custom transformer '{}' write failed: {e}", self.argv[0]))
        })?;

        let mut resp = String::new();
        let n = p
            .stdout
            .read_line(&mut resp)
            .map_err(|e| Error::Transform(format!("custom transformer read failed: {e}")))?;
        if n == 0 {
            return Err(Error::Transform(format!(
                "custom transformer '{}' produced no output (process exited)",
                self.argv[0]
            )));
        }
        let parsed: serde_json::Value = serde_json::from_str(resp.trim_end())
            .map_err(|e| Error::Transform(format!("custom transformer bad output: {e}")))?;
        Ok(json_to_bson(&parsed))
    }
}

/// Minimal Bson -> JSON for the line protocol (scalars, arrays, documents).
fn bson_to_json(b: &Bson) -> serde_json::Value {
    use serde_json::Value as J;
    match b {
        Bson::Null => J::Null,
        Bson::Boolean(v) => J::Bool(*v),
        Bson::Int32(v) => J::from(*v),
        Bson::Int64(v) => J::from(*v),
        Bson::Double(v) => serde_json::Number::from_f64(*v).map(J::Number).unwrap_or(J::Null),
        Bson::String(s) => J::String(s.clone()),
        Bson::Array(a) => J::Array(a.iter().map(bson_to_json).collect()),
        Bson::Document(d) => {
            J::Object(d.iter().map(|(k, v)| (k.clone(), bson_to_json(v))).collect())
        }
        other => J::String(other.to_string()),
    }
}

/// Minimal JSON -> Bson for the line protocol.
fn json_to_bson(v: &serde_json::Value) -> Bson {
    use serde_json::Value as J;
    match v {
        J::Null => Bson::Null,
        J::Bool(b) => Bson::Boolean(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Bson::Int64(i)
            } else {
                Bson::Double(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        J::String(s) => Bson::String(s.clone()),
        J::Array(a) => Bson::Array(a.iter().map(json_to_bson).collect()),
        J::Object(o) => {
            let mut d = bson::Document::new();
            for (k, val) in o {
                d.insert(k.clone(), json_to_bson(val));
            }
            Bson::Document(d)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::Document;

    fn defs(src: &str) -> Vec<CustomTransformerDef> {
        serde_yaml::from_str(src).unwrap()
    }

    // Acceptance: a command-driver custom transformer spawns the executable and
    // exchanges documents with it over stdin/stdout for each record.
    #[test]
    fn command_driver_exchanges_over_stdio() {
        let mut r = Registry::with_builtins();
        // `cat` echoes each line back → identity transform, proving the round trip.
        register_custom(&mut r, &defs("- name: echo\n  driver: command\n  command: [cat]\n")).unwrap();

        let t = r.instantiate("echo", &Default::default(), &crate::hash::HashEngine::new("s")).unwrap();
        let doc = Document::new();
        assert_eq!(
            t.transform(&Bson::String("hello".into()), &doc).unwrap(),
            Bson::String("hello".into())
        );
        // multiple records over the same persistent process.
        assert_eq!(
            t.transform(&Bson::Int64(42), &doc).unwrap(),
            Bson::Int64(42)
        );
    }

    // Acceptance: a record the external process fails to produce causes a clear
    // error.
    #[test]
    fn command_failure_is_a_clear_error() {
        let mut r = Registry::with_builtins();
        register_custom(
            &mut r,
            &defs("- name: dies\n  driver: command\n  command: [sh, -c, 'exit 1']\n"),
        )
        .unwrap();
        let t = r.instantiate("dies", &Default::default(), &crate::hash::HashEngine::new("s")).unwrap();
        let err = t.transform(&Bson::String("x".into()), &Document::new()).unwrap_err();
        assert!(
            err.to_string().contains("custom transformer"),
            "unclear error: {err}"
        );
    }

    // Acceptance: custom transformers appear in the catalog alongside built-ins
    // and can declare their own typed parameters.
    #[test]
    fn appear_in_catalog_with_typed_params() {
        let mut r = Registry::with_builtins();
        register_custom(
            &mut r,
            &defs(
                "- name: greet\n  driver: template\n  template: 'hi {{ name }}'\n  parameters:\n    - name: locale\n      type: string\n      required: true\n",
            ),
        )
        .unwrap();
        assert!(r.names().contains(&"greet"));
        // sits next to built-ins.
        assert!(r.names().contains(&"hash"));
        let doc = r.get("greet").unwrap().doc();
        assert_eq!(doc.parameters.len(), 1);
        assert_eq!(doc.parameters[0].name, "locale");
        assert!(doc.parameters[0].required);

        // template driver renders from the document (locale supplied to satisfy
        // the declared required parameter).
        let raw: crate::config::Params = serde_yaml::from_str("locale: en\n").unwrap();
        let t = r.instantiate("greet", &raw, &crate::hash::HashEngine::new("s")).unwrap();
        let mut d = Document::new();
        d.insert("name", "Sam");
        assert_eq!(
            t.transform(&Bson::Null, &d).unwrap(),
            Bson::String("hi Sam".into())
        );
    }

    #[test]
    fn unknown_driver_is_rejected() {
        let mut r = Registry::new();
        let err = register_custom(&mut r, &defs("- name: x\n  driver: wat\n")).unwrap_err();
        assert!(err.to_string().contains("unknown driver"), "{err}");
    }
}
