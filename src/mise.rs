use crate::model::{
    EnvironmentValue, MiseTask, MiseTool, parse_environment, parse_tasks, parse_tools,
};
use openidea_plugin::{Error, Project};

const PROGRAM: &str = "mise";

pub fn tasks(project: &Project) -> Result<Vec<MiseTask>, String> {
    run(project, &["tasks", "ls", "--json"]).and_then(|text| parse_tasks(&text))
}

pub fn tools(project: &Project) -> Result<Vec<MiseTool>, String> {
    run(project, &["ls", "--json"]).and_then(|text| parse_tools(&text))
}

pub fn environment(project: &Project) -> Result<Vec<EnvironmentValue>, String> {
    run(project, &["env", "--json"]).and_then(|text| parse_environment(&text))
}

pub fn trust(project: &Project) -> Result<(), String> {
    run(project, &["--yes", "trust"]).map(|_| ())
}

pub fn requires_trust(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("not trusted") || error.contains("untrusted-config")
}

fn run(project: &Project, args: &[&str]) -> Result<String, String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let output = project
        .run_process(PROGRAM, &args, None)
        .map_err(friendly_error)?;
    if output.status == Some(0) {
        return Ok(output.stdout);
    }
    let detail = if output.stderr.trim().is_empty() {
        output.stdout.trim()
    } else {
        output.stderr.trim()
    };
    let detail = truncate(detail, 800);
    Err(match output.status {
        Some(status) if detail.is_empty() => format!("mise exited with status {status}"),
        Some(status) => format!("mise exited with status {status}: {detail}"),
        None if detail.is_empty() => "mise was terminated".to_owned(),
        None => format!("mise was terminated: {detail}"),
    })
}

fn friendly_error(error: Error) -> String {
    match error {
        Error::Denied(_) => "permission to run mise is not granted".to_owned(),
        Error::NotFound(_) => "mise was not found on PATH".to_owned(),
        Error::Cancelled => "mise was cancelled".to_owned(),
        other => other.to_string(),
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_mise_trust_errors() {
        assert!(requires_trust(
            "mise ERROR Config files in /project/mise.toml are not trusted."
        ));
        assert!(requires_trust("untrusted-config: /project/.mise.toml"));
        assert!(!requires_trust("mise was not found on PATH"));
    }
}
