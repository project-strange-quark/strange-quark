#!/usr/bin/env bash
set -eu
cp -R "$(dirname "$0")/files/." .
# The skill works on a git branch, so give the copied project a clean history.
if [ ! -d .git ]; then
  git init -q -b main
  git add -A
  git -c user.name=fixture -c user.email=fixture@example.invalid commit -qm "initial import"
fi
