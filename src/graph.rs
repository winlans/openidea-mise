use crate::model::MiseTask;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskGraph {
    pub nodes: Vec<TaskGraphNode>,
    pub edges: Vec<TaskGraphEdge>,
    pub cycles: Vec<Vec<String>>,
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskGraphNode {
    pub name: String,
    pub description: String,
    pub source: String,
    pub cyclic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskGraphEdge {
    pub source: String,
    pub target: String,
    pub reference: String,
    pub kind: TaskEdgeKind,
    pub missing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskEdgeKind {
    Depends,
    DependsPost,
    WaitFor,
}

pub fn build(tasks: &[MiseTask]) -> TaskGraph {
    let tasks_by_name: BTreeMap<&str, &MiseTask> = tasks
        .iter()
        .map(|task| (task.name.as_str(), task))
        .collect();
    let aliases: BTreeMap<&str, &str> = tasks
        .iter()
        .flat_map(|task| {
            task.aliases
                .iter()
                .map(move |alias| (alias.as_str(), task.name.as_str()))
        })
        .collect();
    let mut edges = Vec::new();
    let mut missing = BTreeSet::new();

    for task in tasks_by_name.values() {
        add_edges(
            &mut edges,
            &mut missing,
            task,
            &task.depends,
            TaskEdgeKind::Depends,
            &tasks_by_name,
            &aliases,
        );
        add_edges(
            &mut edges,
            &mut missing,
            task,
            &task.depends_post,
            TaskEdgeKind::DependsPost,
            &tasks_by_name,
            &aliases,
        );
        add_edges(
            &mut edges,
            &mut missing,
            task,
            &task.wait_for,
            TaskEdgeKind::WaitFor,
            &tasks_by_name,
            &aliases,
        );
    }
    edges.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.reference.cmp(&right.reference))
    });

    let cycles = find_cycles(tasks_by_name.keys().copied(), &edges);
    let cyclic: BTreeSet<&str> = cycles
        .iter()
        .flat_map(|cycle| cycle.iter().map(String::as_str))
        .collect();
    let nodes = tasks_by_name
        .into_values()
        .map(|task| TaskGraphNode {
            name: task.name.clone(),
            description: task.description.clone(),
            source: task.source.clone(),
            cyclic: cyclic.contains(task.name.as_str()),
        })
        .collect();

    TaskGraph {
        nodes,
        edges,
        cycles,
        missing: missing.into_iter().collect(),
    }
}

fn add_edges(
    edges: &mut Vec<TaskGraphEdge>,
    missing_names: &mut BTreeSet<String>,
    task: &MiseTask,
    references: &[String],
    kind: TaskEdgeKind,
    tasks: &BTreeMap<&str, &MiseTask>,
    aliases: &BTreeMap<&str, &str>,
) {
    for reference in references {
        let target = aliases
            .get(reference.as_str())
            .copied()
            .unwrap_or(reference);
        let missing = !tasks.contains_key(target);
        if missing {
            missing_names.insert(reference.clone());
        }
        edges.push(TaskGraphEdge {
            source: task.name.clone(),
            target: target.to_owned(),
            reference: reference.clone(),
            kind,
            missing,
        });
    }
}

fn find_cycles<'a>(
    names: impl Iterator<Item = &'a str>,
    edges: &[TaskGraphEdge],
) -> Vec<Vec<String>> {
    let names: Vec<&str> = names.collect();
    let known: BTreeSet<&str> = names.iter().copied().collect();
    let mut forward: BTreeMap<&str, Vec<&str>> =
        names.iter().map(|name| (*name, Vec::new())).collect();
    let mut reverse = forward.clone();

    for edge in edges.iter().filter(|edge| !edge.missing) {
        let source = edge.source.as_str();
        let target = edge.target.as_str();
        if known.contains(source) && known.contains(target) {
            forward.entry(source).or_default().push(target);
            reverse.entry(target).or_default().push(source);
        }
    }
    for targets in forward.values_mut().chain(reverse.values_mut()) {
        targets.sort_unstable();
        targets.dedup();
    }

    let mut visited = BTreeSet::new();
    let mut order = Vec::with_capacity(names.len());
    for name in &names {
        finish_order(name, &forward, &mut visited, &mut order);
    }

    visited.clear();
    let mut cycles = Vec::new();
    for name in order.into_iter().rev() {
        if visited.contains(name) {
            continue;
        }
        let mut component = Vec::new();
        collect_component(name, &reverse, &mut visited, &mut component);
        component.sort_unstable();
        let self_cycle = component.len() == 1
            && forward
                .get(component[0])
                .is_some_and(|targets| targets.contains(&component[0]));
        if component.len() > 1 || self_cycle {
            cycles.push(component.into_iter().map(str::to_owned).collect());
        }
    }
    cycles.sort();
    cycles
}

fn finish_order<'a>(
    name: &'a str,
    graph: &BTreeMap<&'a str, Vec<&'a str>>,
    visited: &mut BTreeSet<&'a str>,
    order: &mut Vec<&'a str>,
) {
    if !visited.insert(name) {
        return;
    }
    if let Some(targets) = graph.get(name) {
        for target in targets {
            finish_order(target, graph, visited, order);
        }
    }
    order.push(name);
}

fn collect_component<'a>(
    name: &'a str,
    graph: &BTreeMap<&'a str, Vec<&'a str>>,
    visited: &mut BTreeSet<&'a str>,
    component: &mut Vec<&'a str>,
) {
    if !visited.insert(name) {
        return;
    }
    component.push(name);
    if let Some(targets) = graph.get(name) {
        for target in targets {
            collect_component(target, graph, visited, component);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(name: &str) -> MiseTask {
        MiseTask {
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn builds_all_edge_kinds_and_reports_missing_tasks() {
        let mut build_task = task("build");
        build_task.depends = vec!["lint".into(), "unknown".into()];
        build_task.depends_post = vec!["notify".into()];
        build_task.wait_for = vec!["server".into()];
        let graph = build(&[build_task, task("lint"), task("notify"), task("server")]);

        assert_eq!(graph.edges.len(), 4);
        assert_eq!(graph.missing, ["unknown"]);
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.kind == TaskEdgeKind::DependsPost)
        );
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.kind == TaskEdgeKind::WaitFor)
        );
    }

    #[test]
    fn resolves_task_aliases_to_graph_nodes() {
        let mut lint = task("lint");
        lint.aliases = vec!["l".into()];
        let mut build_task = task("build");
        build_task.depends = vec!["l".into()];

        let graph = build(&[build_task, lint]);
        assert_eq!(graph.edges[0].target, "lint");
        assert_eq!(graph.edges[0].reference, "l");
        assert!(!graph.edges[0].missing);
    }

    #[test]
    fn detects_strongly_connected_and_self_cycles() {
        let mut first = task("first");
        first.depends = vec!["second".into()];
        let mut second = task("second");
        second.wait_for = vec!["first".into()];
        let mut self_cycle = task("self");
        self_cycle.depends_post = vec!["self".into()];
        let graph = build(&[task("clean"), first, second, self_cycle]);

        assert_eq!(
            graph.cycles,
            [
                vec!["first".to_owned(), "second".to_owned()],
                vec!["self".to_owned()]
            ]
        );
        assert!(
            graph
                .nodes
                .iter()
                .filter(|node| node.cyclic)
                .map(|node| node.name.as_str())
                .eq(["first", "second", "self"])
        );
    }
}
