# Paseo Hub GitHub agent

These Hub workflows are the Paseo replacement for the GitHub Actions
`Agent Serviceuser` pi agent. They run Codex with the DeepSeek provider
(`deepseek-chat`) from GitHub events, gated by labels instead of assignees.

Hub refuses providers that cannot preapprove exact MCP tools for unattended
execution, so Pi cannot be used here. Codex is enabled on the daemon with a
custom DeepSeek provider; see [Runtime setup](#runtime-setup).

## Labels

| Label | Meaning |
| --- | --- |
| `pi-agent-assignee` | The agent owns implementation: fix/implement an issue or continue a pull request. |
| `pi-agent-reviewer` | The agent reviews a pull request only and does not change files. On an issue it replies with analysis only. |

The labels are the new assignment mechanism. Adding a role label starts the agent;
comment and review events only start it when the item already carries a role
label. The two labels are intended to be mutually exclusive.

## Workflows

Label events:

| Workflow | Event | Label |
| --- | --- | --- |
| `agent-serviceuser-assignee-issue-label.yml` | `github.issue_label_added` | `pi-agent-assignee` |
| `agent-serviceuser-assignee-pull-request-label.yml` | `github.pull_request_label_added` | `pi-agent-assignee` |
| `agent-serviceuser-reviewer-pull-request-label.yml` | `github.pull_request_label_added` | `pi-agent-reviewer` |

Comment and review events (each exists once for `assignee` and once for
`reviewer`):

| Event | Required label |
| --- | --- |
| `github.issue_comment_created` | `pi-agent-assignee` or `pi-agent-reviewer` |
| `github.pull_request_comment_created` | `pi-agent-assignee` or `pi-agent-reviewer` |
| `github.pull_request_review_comment` | `pi-agent-assignee` or `pi-agent-reviewer` |
| `github.pull_request_review` | `pi-agent-assignee` or `pi-agent-reviewer` |

CI failures also start the assignee workflow. `.github/workflows/notify-paseo-on-ci-failure.yml`
posts a pull-request comment from `github-actions[bot]` containing
`@agent-serviceuser`. `agent-serviceuser-ci-failure.yml` matches only that bot
comment, requires the `pi-agent-assignee` label, and resumes the same
pull-request conversation.

Hub filters cannot express "either label", so each human comment/review event
has one workflow per label. Keep the labels mutually exclusive to avoid two runs
on the same item.

All workflows use the `nikwebr-github` connection and the `dev`
environment from `hub.yml`. The shared prompt lives in `workflows/partials/`.

## GitHub authorship

The steps intentionally omit the Hub `github` authority block. That keeps Hub
from injecting the GitHub App token into the agent environment, so `gh` and `git`
use the daemon user's existing login: `agent-serviceuser`. Commits, pull requests,
comments, and reviews created by the agent are therefore authored as
`agent-serviceuser` rather than the Hub App.

## Runtime setup

The daemon needs Codex plus a DeepSeek provider configuration:

- Install `@openai/codex` so `codex` is on the daemon `PATH`.
- Configure `~/.codex/config.toml` with a `deepseek` model provider using
  `wire_api = "responses"` and `env_key = "DEEPSEEK_API_KEY"`.
- Enable the Paseo `codex` provider and give it `DEEPSEEK_API_KEY` (for example in
  `~/.paseo/config.json` under `agents.providers.codex.env`).
- Add `deepseek-flash` to the provider models, then reload the daemon.

## Disabled GitHub Actions workflow

The GitHub Actions pi agent is disabled but its configuration is preserved:

- `.github/workflows/agent-serviceuser.md` remains the gh-aw source.
- The compiled workflow was moved to
  `old_github_workflows/agent-serviceuser.lock.yml` so GitHub no longer runs it.
- The live workflow was also disabled with
  `gh workflow disable "Agent Serviceuser"`.

To re-enable it, move the lock file back to `.github/workflows/`, run
`gh workflow enable "Agent Serviceuser"`, and remove the matching Hub workflows so
the same item does not start both agents.

## Deployment

Deploy with:

```bash
paseo hub deploy -p default
```

The bundle uses the `nikwebr-github` connection, which has access to
`nikwebr/react-native-s3-bg-uploader`. It is deployed to the `default` project
with 12 workflows.
