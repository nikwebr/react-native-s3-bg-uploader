# Paseo Hub GitHub agent

These Hub workflows are the Paseo replacement for the GitHub Actions
`Agent Serviceuser` pi agent. They start the same Pi model
(`deepseek/deepseek-flash`) when `@agent-serviceuser` is mentioned on an issue, a
pull-request conversation, a review comment, or a pull-request review body.

| Workflow | GitHub event |
| --- | --- |
| `agent-serviceuser-issue-comment.yml` | `github.issue_comment_created` |
| `agent-serviceuser-pull-request-comment.yml` | `github.pull_request_comment_created` |
| `agent-serviceuser-review-comment.yml` | `github.pull_request_review_comment` |
| `agent-serviceuser-review.yml` | `github.pull_request_review` |

All four use the `agent-serviceuser-github` connection and the `dev` environment
from `hub.yml`. The shared prompt lives in `workflows/partials/`.

## Disabled GitHub Actions workflow

The GitHub Actions pi agent is disabled but its configuration is preserved:

- `.github/workflows/agent-serviceuser.md` remains the gh-aw source.
- The compiled workflow was moved to
  `old_github_workflows/agent-serviceuser.lock.yml` so GitHub no longer runs it.
- The live workflow was also disabled with
  `gh workflow disable "Agent Serviceuser"`.

To re-enable it, move the lock file back to `.github/workflows/`, run
`gh workflow enable "Agent Serviceuser"`, and remove the matching Hub workflows so
the same mention does not start both agents.

## Deployment prerequisite

Deploy with:

```bash
paseo hub deploy -p default
```

Hub currently rejects the bundle with:

```
filters.repo: "nikwebr/react-native-s3-bg-uploader" does not match any GitHub repository (connected: none)
```

The `agent-serviceuser-github` connection in Hub has no repositories. The repo
owner (`nikwebr`) must install the Hub GitHub App on
`nikwebr/react-native-s3-bg-uploader`, or grant that repository to the existing
installation, before the bundle can be activated.

## Scope limit

Hub has no GitHub `assigned` or `review_requested` event, so the part of the old
pi agent that started on assignment or reviewer request cannot be represented
here. The mention-driven issue, pull-request, review-comment, and review-body
triggers are covered. The old GitHub Actions workflow can be re-enabled if
assignment-triggered runs are still needed.
