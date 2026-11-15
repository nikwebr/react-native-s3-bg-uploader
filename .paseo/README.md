# Paseo Hub GitHub agent

These Hub workflows are the Paseo replacement for the GitHub Actions
`Agent Serviceuser` pi agent. They start the same Pi model
(`deepseek/deepseek-flash`) from GitHub events, gated by labels instead of
assignees.

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

Hub filters cannot express "either label", so each comment/review event has one
workflow per label. Keep the labels mutually exclusive to avoid two runs on the
same item.

All workflows use the `nikwebr-github` connection and the `dev`
environment from `hub.yml`. The shared prompt lives in `workflows/partials/`.

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
with 11 workflows.
