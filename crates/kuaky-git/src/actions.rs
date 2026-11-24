//! GitHub Actions con `gh`: workflows, últimas ejecuciones y lanzar uno a mano
//! (`workflow_dispatch`, con sus inputs leídos del YAML del repositorio).

use serde::Deserialize;
use yaml_rust2::{Yaml, YamlLoader};

#[derive(Debug, Clone, Deserialize)]
pub struct Workflow {
    pub id: u64,
    pub name: String,
    /// `.github/workflows/ci.yml`
    pub path: String,
    /// `active`, `disabled_manually`…
    #[serde(default)]
    pub state: String,
    /// Se puede lanzar a mano (`on: workflow_dispatch`); se llena leyendo el YAML.
    #[serde(skip)]
    pub dispatch: Option<Vec<WorkflowInput>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorkflowInput {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub default: String,
    /// `string`, `choice`, `boolean`, `environment`, `number`.
    pub kind: String,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub database_id: u64,
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub display_title: String,
    #[serde(default)]
    pub workflow_name: String,
    #[serde(default)]
    pub workflow_database_id: u64,
    /// `queued`, `in_progress`, `completed`, `waiting`, `requested`, `pending`.
    #[serde(default)]
    pub status: String,
    /// `success`, `failure`, `cancelled`, `skipped`… (vacío mientras corre).
    #[serde(default)]
    pub conclusion: String,
    #[serde(default)]
    pub head_branch: String,
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub url: String,
}

impl WorkflowRun {
    pub fn running(&self) -> bool {
        self.status != "completed"
    }
}

/// Inputs de `workflow_dispatch`, o `None` si el workflow no se puede lanzar a mano.
pub fn dispatch_inputs(yaml: &str) -> Option<Vec<WorkflowInput>> {
    let docs = YamlLoader::load_from_str(yaml).ok()?;
    let doc = docs.first()?;
    // `on:` (algunos lectores YAML 1.1 lo guardan como la clave booleana `true`).
    let on = match &doc["on"] {
        v if !v.is_badvalue() => v,
        _ => doc.as_hash()?.get(&Yaml::Boolean(true))?,
    };
    let trigger = match on {
        Yaml::String(s) => return (s == "workflow_dispatch").then(Vec::new),
        Yaml::Array(list) => return list.iter().any(|v| v.as_str() == Some("workflow_dispatch")).then(Vec::new),
        Yaml::Hash(_) => &on["workflow_dispatch"],
        _ => return None,
    };
    if trigger.is_badvalue() {
        return None;
    }
    let Some(inputs) = trigger["inputs"].as_hash() else { return Some(Vec::new()) };
    let text = |v: &Yaml| match v {
        Yaml::String(s) | Yaml::Real(s) => s.clone(),
        Yaml::Integer(n) => n.to_string(),
        Yaml::Boolean(b) => b.to_string(),
        _ => String::new(),
    };
    Some(
        inputs
            .iter()
            .filter_map(|(name, spec)| {
                Some(WorkflowInput {
                    name: name.as_str()?.to_string(),
                    description: text(&spec["description"]),
                    required: spec["required"].as_bool().unwrap_or(false),
                    default: text(&spec["default"]),
                    kind: spec["type"].as_str().unwrap_or("string").to_string(),
                    options: spec["options"].as_vec().map(|o| o.iter().map(text).collect()).unwrap_or_default(),
                })
            })
            .collect(),
    )
}

impl crate::Repo {
    /// Workflows del repositorio, con sus inputs si se pueden lanzar a mano.
    pub fn workflows(&self) -> anyhow::Result<Vec<Workflow>> {
        let out = crate::gh_in(self.root(), &["workflow", "list", "--all", "--json", "id,name,path,state"])?;
        let mut list: Vec<Workflow> = serde_json::from_str(&out)?;
        for w in &mut list {
            let file = self.root().join(&w.path);
            w.dispatch = std::fs::read_to_string(&file).ok().and_then(|y| dispatch_inputs(&y));
        }
        list.sort_by_key(|w| (w.dispatch.is_none(), w.state != "active", w.name.to_lowercase()));
        Ok(list)
    }

    /// Últimas ejecuciones (de todos los workflows).
    pub fn workflow_runs(&self, limit: usize) -> anyhow::Result<Vec<WorkflowRun>> {
        let out = crate::gh_in(
            self.root(),
            &[
                "run",
                "list",
                "--limit",
                &limit.to_string(),
                "--json",
                "databaseId,number,displayTitle,workflowName,workflowDatabaseId,status,conclusion,headBranch,event,createdAt,url",
            ],
        )?;
        Ok(serde_json::from_str(&out)?)
    }

    /// Lanza un workflow en `branch` con sus inputs (`gh workflow run`).
    pub fn run_workflow(&self, workflow: &Workflow, branch: &str, inputs: &[(String, String)]) -> anyhow::Result<()> {
        let id = workflow.id.to_string();
        let mut args: Vec<String> = vec!["workflow".into(), "run".into(), id, "--ref".into(), branch.into()];
        for (k, v) in inputs {
            args.push("-f".into());
            args.push(format!("{k}={v}"));
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        crate::gh_in(self.root(), &args)?;
        Ok(())
    }

    /// Vuelve a correr una ejecución (solo los jobs que fallaron, si `failed`).
    pub fn rerun(&self, run: u64, failed: bool) -> anyhow::Result<()> {
        let id = run.to_string();
        let mut args = vec!["run", "rerun", id.as_str()];
        if failed {
            args.push("--failed");
        }
        crate::gh_in(self.root(), &args)?;
        Ok(())
    }

    pub fn cancel_run(&self, run: u64) -> anyhow::Result<()> {
        crate::gh_in(self.root(), &["run", "cancel", &run.to_string()])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::dispatch_inputs;

    #[test]
    fn release_workflow_asks_for_a_version() {
        let inputs = dispatch_inputs(include_str!("../../../.github/workflows/release.yml")).unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "version");
        assert!(inputs[0].required);
    }

    #[test]
    fn reads_dispatch_inputs() {
        assert_eq!(dispatch_inputs("on:\n  push:\n    branches: [main]\n"), None);
        assert_eq!(dispatch_inputs("on: workflow_dispatch\n"), Some(vec![]));
        assert_eq!(dispatch_inputs("on: [push, workflow_dispatch]\n"), Some(vec![]));
        assert_eq!(dispatch_inputs("on:\n  workflow_dispatch:\n  push:\n"), Some(vec![]));
        let y = "name: Deploy\non:\n  workflow_dispatch:\n    inputs:\n      env:\n        description: Where\n        required: true\n        type: choice\n        options: [staging, prod]\n        default: staging\n      dry:\n        type: boolean\n        default: false\n";
        let inputs = dispatch_inputs(y).unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].name, "env");
        assert!(inputs[0].required);
        assert_eq!(inputs[0].options, ["staging", "prod"]);
        assert_eq!(inputs[0].default, "staging");
        assert_eq!(inputs[1].kind, "boolean");
        assert_eq!(inputs[1].default, "false");
    }
}
