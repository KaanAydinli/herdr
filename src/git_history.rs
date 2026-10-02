use std::{path::Path, process::Stdio, time::Duration};

use tokio::io::AsyncReadExt;

use crate::api::schema::{ErrorBody, ErrorResponse, SuccessResponse};
use crate::api::schema::{
    GitCommit, GitHistory, GitHistoryParams, Method, Request, ResponseResult,
};

fn encode_error(id: String, code: &str, message: impl Into<String>) -> String {
    serde_json::to_string(&ErrorResponse {
        id,
        error: ErrorBody {
            code: code.into(),
            message: message.into(),
        },
    })
    .unwrap_or_default()
}

fn encode_success(id: String, result: ResponseResult) -> String {
    serde_json::to_string(&SuccessResponse { id, result }).unwrap_or_default()
}

const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;

pub(crate) fn start(request: Request, respond_to: std::sync::mpsc::Sender<String>) {
    let Method::GitHistory(params) = request.method else {
        return;
    };
    tokio::spawn(async move {
        let response = match tokio::time::timeout(Duration::from_secs(10), read(&params)).await {
            Ok(Ok(history)) => encode_success(request.id, ResponseResult::GitHistory { history }),
            Ok(Err(message)) => encode_error(request.id, "git_history_unavailable", message),
            Err(_) => encode_error(request.id, "git_history_timeout", "Git history timed out"),
        };
        let _ = respond_to.send(response);
    });
}

fn valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn git(cwd: &Path, args: &[&str]) -> Result<(bool, Vec<u8>), String> {
    let mut command = crate::noninteractive_process::command("git");
    command
        .current_dir(cwd)
        .args(["--no-pager", "--no-optional-locks"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|err| format!("Cannot run Git: {err}"))?;
    let stdout = child.stdout.take().ok_or("Git output unavailable")?;
    let mut output = Vec::new();
    let mut reader = stdout.take(OUTPUT_LIMIT + 1);
    reader
        .read_to_end(&mut output)
        .await
        .map_err(|err| err.to_string())?;
    if output.len() as u64 > OUTPUT_LIMIT {
        let _ = child.kill().await;
        return Err("Git history output is too large".into());
    }
    Ok((
        child.wait().await.map_err(|err| err.to_string())?.success(),
        output,
    ))
}

async fn read(params: &GitHistoryParams) -> Result<GitHistory, String> {
    let cwd = Path::new(&params.cwd);
    if !cwd.is_absolute() || !(1..=200).contains(&params.limit) {
        return Err("History requires an absolute directory and a limit between 1 and 200".into());
    }
    if params
        .revision
        .as_deref()
        .is_some_and(|oid| !valid_oid(oid))
    {
        return Err("History revision must be a full commit ID".into());
    }
    if !git(cwd, &["rev-parse", "--git-dir"]).await?.0 {
        return Err("Not a Git repository".into());
    }
    let mut history = GitHistory {
        head: None,
        commits: Vec::new(),
        has_more: false,
        unchanged: false,
    };
    let (exists, head) = git(
        cwd,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            params.revision.as_deref().unwrap_or("HEAD"),
        ],
    )
    .await?;
    if !exists {
        if params.revision.is_some() {
            return Err("Commit is no longer available".into());
        }
        return Ok(history);
    }
    let head = String::from_utf8_lossy(&head).trim().to_owned();
    if !valid_oid(&head) {
        return Err("Git returned an invalid commit ID".into());
    }
    history.head = Some(head.clone());
    if params.skip == 0 && params.known_head.as_ref() == Some(&head) {
        history.unchanged = true;
        return Ok(history);
    }
    let limit = format!("--max-count={}", params.limit + 1);
    let skip = format!("--skip={}", params.skip);
    let (success, output) = git(
        cwd,
        &[
            "log",
            "--topo-order",
            "--no-color",
            "--no-decorate",
            "--no-show-signature",
            "--no-notes",
            "--root",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--diff-merges=first-parent",
            "--encoding=UTF-8",
            "-z",
            "--format=commit%x00%H%x00%P%x00%s",
            "--numstat",
            &limit,
            &skip,
            &head,
            "--",
        ],
    )
    .await?;
    if !success {
        return Err("Unable to read Git history".into());
    }
    history.commits = parse_log(&output)?;
    history.has_more = history.commits.len() > usize::from(params.limit);
    history.commits.truncate(usize::from(params.limit));
    Ok(history)
}

fn parse_log(output: &[u8]) -> Result<Vec<GitCommit>, String> {
    let mut fields = output.split(|byte| *byte == 0);
    let mut commits: Vec<GitCommit> = Vec::new();
    while let Some(field) = fields.next() {
        let field = field.strip_prefix(b"\n").unwrap_or(field);
        if field.is_empty() {
            continue;
        }
        if field == b"commit" {
            let id = fields.next().ok_or("Missing commit ID")?;
            let parents = fields.next().ok_or("Missing commit parents")?;
            let subject = fields.next().ok_or("Missing commit subject")?;
            let id = String::from_utf8_lossy(id).into_owned();
            if !valid_oid(&id) {
                return Err("Invalid commit ID".into());
            }
            commits.push(GitCommit {
                id,
                parents: String::from_utf8_lossy(parents)
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect(),
                subject: String::from_utf8_lossy(subject)
                    .chars()
                    .map(|ch| if ch.is_control() { ' ' } else { ch })
                    .collect(),
                additions: 0,
                deletions: 0,
                binary_files: 0,
            });
        } else if let Some(commit) = commits.last_mut() {
            let mut stats = field.splitn(3, |byte| *byte == b'\t');
            let added = stats.next().ok_or("Missing additions")?;
            let deleted = stats.next().ok_or("Missing deletions")?;
            if stats.next().is_none() {
                return Err("Missing changed path".into());
            }
            if added == b"-" || deleted == b"-" {
                commit.binary_files = commit.binary_files.saturating_add(1);
            } else {
                let count = |value| {
                    String::from_utf8_lossy(value)
                        .parse::<u64>()
                        .map_err(|_| "Invalid line count")
                };
                commit.additions = commit.additions.saturating_add(count(added)?);
                commit.deletions = commit.deletions.saturating_add(count(deleted)?);
            }
        }
    }
    Ok(commits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Repository(PathBuf);

    impl Repository {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "herdr-history-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            let repo = Self(path);
            repo.run(&["init", "--quiet", "--initial-branch=main"]);
            repo
        }

        fn run(&self, args: &[&str]) {
            let output = std::process::Command::new("git")
                .current_dir(&self.0)
                .args([
                    "-c",
                    "user.name=History Test",
                    "-c",
                    "user.email=history@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        fn params(&self) -> GitHistoryParams {
            GitHistoryParams {
                cwd: self.0.to_string_lossy().into_owned(),
                revision: None,
                known_head: None,
                skip: 0,
                limit: 100,
            }
        }

        fn commit(&self, subject: &str) {
            self.run(&["add", "."]);
            self.run(&["commit", "--quiet", "-m", subject]);
        }
    }

    impl Drop for Repository {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn history_handles_roots_merges_binary_files_and_pagination() {
        let repo = Repository::new();
        assert!(read(&repo.params()).await.unwrap().commits.is_empty());
        std::fs::write(repo.0.join("file"), "one\ntwo\n").unwrap();
        repo.commit("初始 commit");
        repo.run(&["checkout", "--quiet", "-b", "feature"]);
        std::fs::write(repo.0.join("odd name file"), "three\n").unwrap();
        std::fs::write(repo.0.join("binary"), b"a\0b").unwrap();
        repo.commit("feature");
        repo.run(&["checkout", "--quiet", "main"]);
        std::fs::write(repo.0.join("file"), "one\nchanged\nthree\n").unwrap();
        repo.commit("main");
        repo.run(&["merge", "--quiet", "--no-ff", "feature", "-m", "merge"]);
        repo.run(&["config", "log.showroot", "false"]);
        repo.run(&["config", "log.showSignature", "true"]);
        let history = read(&repo.params()).await.unwrap();
        assert_eq!(history.commits.len(), 4);
        assert_eq!(history.commits[0].parents.len(), 2);
        assert_eq!(
            (
                history.commits[0].additions,
                history.commits[0].deletions,
                history.commits[0].binary_files
            ),
            (1, 0, 1)
        );
        let root = history.commits.last().unwrap();
        assert!(root.parents.is_empty());
        assert_eq!(root.subject, "初始 commit");
        assert_eq!((root.additions, root.deletions), (2, 0));
        let main = history
            .commits
            .iter()
            .find(|commit| commit.subject == "main")
            .unwrap();
        assert_eq!((main.additions, main.deletions), (2, 1));
        let mut params = repo.params();
        params.limit = 2;
        let first = read(&params).await.unwrap();
        assert!(first.has_more);
        params.skip = 2;
        params.revision = first.head;
        let second = read(&params).await.unwrap();
        assert!(!second.has_more);
        assert_eq!([first.commits, second.commits].concat(), history.commits);
        params.skip = 0;
        params.known_head = history.head;
        let cached = read(&params).await.unwrap();
        assert!(cached.unchanged);
        assert!(cached.commits.is_empty());
    }

    #[tokio::test]
    async fn history_rejects_invalid_sources_and_revisions() {
        let repo = Repository::new();
        let mut params = repo.params();
        params.revision = Some("--all".into());
        assert!(read(&params).await.is_err());
        params.revision = None;
        params.cwd = ".".into();
        assert!(read(&params).await.is_err());
        params.cwd = repo.0.join("missing").to_string_lossy().into_owned();
        assert!(read(&params).await.is_err());
        params = repo.params();
        params.limit = 201;
        assert!(read(&params).await.is_err());
    }
}
