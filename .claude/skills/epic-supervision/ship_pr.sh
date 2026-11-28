#!/usr/bin/env bash
# Open, watch and merge an approved epic branch as one pull request.
# Usage: ship_pr.sh <branch> <issue> <title> <body-file>
# Run from any clone of the repository. Stops before merging if the branch is
# behind main, the body does not reference the issue, or a check fails.
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: $0 <branch> <issue> <title> <body-file>" >&2
  exit 64
fi
branch=$1 issue=$2 title=$3 body=$4
repo=${GENAIMAGE_REPO:-eemaldi/genaimage}

# Prints the performance job's "Release binary size" table row for a commit.
binary_size_row() {
  local id row
  for id in $(gh run list --repo "$repo" --commit "$1" --json databaseId --jq '.[].databaseId' 2>/dev/null); do
    row=$(gh run view "$id" --repo "$repo" --log 2>/dev/null | grep -o -m 1 '| Release binary size.*' | tr -d '\r') || true
    if [ -n "$row" ]; then
      echo "$row"
      return 0
    fi
  done
}

git fetch -q origin
if ! git merge-base --is-ancestor origin/main "origin/$branch"; then
  echo "origin/$branch is behind origin/main; start a fresh developer to rebase." >&2
  exit 2
fi
if ! grep -Eq "(Closes|Refs) #$issue\b" "$body"; then
  echo "$body must contain 'Closes #$issue' or 'Refs #$issue'." >&2
  exit 2
fi

url=$(gh pr create --repo "$repo" --base main --head "$branch" \
  --title "$title" --body-file "$body" | tail -n 1)
echo "Opened $url"

# Starting the watch immediately can report "no checks" and exit.
sleep 20
gh pr checks "$url" --watch
gh pr merge "$url" --merge

# GitHub closes linked issues a few seconds after the merge.
for _ in 1 2 3 4 5 6; do
  state=$(gh issue view "$issue" --repo "$repo" --json state --jq .state)
  [ "$state" = "CLOSED" ] && break
  sleep 5
done
echo "Issue #$issue is $state."
if [ "$state" != "CLOSED" ]; then
  echo "Close it with a comment if it should not stay open." >&2
fi
git fetch -q origin
echo "origin/main is now $(git log --oneline -1 origin/main)"

head_sha=$(gh pr view "$url" --repo "$repo" --json headRefOid --jq .headRefOid 2>/dev/null || true)
row=$(binary_size_row "$head_sha" || true)
if [ -z "$row" ]; then
  echo "Binary size: not found (check the performance job)"
else
  echo "Binary size: $row"
  case $row in
    *"OVER BUDGET"*) echo "WARNING: the release binary is over its size budget." >&2 ;;
  esac
fi
