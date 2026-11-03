#!/usr/bin/env bash
# Keep AK/monocode_enhancements on top of the newest upstream native preview.
#
#   scripts/sync-upstream.sh            fetch and report what is new
#   scripts/sync-upstream.sh --rebase   rebase the branch onto the newest native-v* tag
#   scripts/sync-upstream.sh --rebase --install
#                                       ...then check, test, build and install the app
#   scripts/sync-upstream.sh --rebase --push
#                                       ...then force-push the branch to the fork
#   scripts/sync-upstream.sh --main     fast-forward the fork's main to upstream/main
#
# Remotes: `upstream` is hardbeat920/monocode, `origin` is the fork.
set -euo pipefail
cd "$(dirname "$0")/.."
branch=AK/monocode_enhancements
rebase=0 install=0 push=0 main=0
for arg in "$@"; do
  case "$arg" in
    --rebase) rebase=1 ;;
    --install) install=1 ;;
    --push) push=1 ;;
    --main) main=1 ;;
    *) echo "Unknown option: $arg" >&2; exit 2 ;;
  esac
done

git fetch upstream --tags --prune --quiet
latest=$(git tag -l 'native-v*' | sort -V | tail -1)
[ -n "$latest" ] || { echo 'No native-v* tag found on upstream.' >&2; exit 1; }
behind_tag=$(git rev-list --count "$branch..$latest")
behind_main=$(git rev-list --count "$branch..upstream/main")
echo "Newest upstream preview: $latest"
echo "  $branch is missing $behind_tag commits of that release and $behind_main of upstream/main."

if [ "$main" = 1 ]; then
  git push origin refs/remotes/upstream/main:refs/heads/main
  git push origin --tags --quiet
fi

if [ "$rebase" = 0 ]; then
  [ "$behind_tag" = 0 ] && echo 'Up to date with the newest preview.' || echo 'Run with --rebase to apply it.'
  exit 0
fi
if [ "$behind_tag" = 0 ]; then
  echo 'Already on top of the newest preview.'
else
  [ "$(git rev-parse --abbrev-ref HEAD)" = "$branch" ] || git checkout "$branch"
  [ -z "$(git status --porcelain)" ] || { echo 'Commit or discard local changes first.' >&2; exit 1; }
  git branch -f "backup/before-$latest" "$branch"
  echo "Rebasing onto $latest (backup: backup/before-$latest)"
  if ! git rebase "$latest"; then
    echo 'Rebase stopped on a conflict. Resolve it, then run: git rebase --continue' >&2
    echo "Or run: git rebase --abort. Your work is also on backup/before-$latest." >&2
    exit 1
  fi
fi

if [ "$install" = 1 ]; then
  manifest=crates/monocode-gpui/Cargo.toml
  cargo clippy --locked --manifest-path "$manifest" -p monocode-core -p monocode-gpui --profile native-dev -- -D warnings
  cargo test --locked --manifest-path "$manifest" -p monocode-core --profile native-dev
  cargo test --locked --manifest-path "$manifest" -p monocode-gpui --profile native-dev -- --test-threads=1
  CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}" npm run native:build
  ditto "crates/monocode-gpui/target/release/Monocode Native.app" "/Applications/Monocode Native.app"
  codesign --verify --deep "/Applications/Monocode Native.app"
  echo 'Installed. Restart Monocode Native.'
fi

if [ "$push" = 1 ]; then
  git push --force-with-lease origin "$branch"
fi
