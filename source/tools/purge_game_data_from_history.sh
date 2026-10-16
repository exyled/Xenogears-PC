#!/bin/sh
# Removes the game data from every commit of the GitHub repository's history, keeping only the player, the readme,
# .gitignore and the Saves folder. It works on a scratch copy, so the game folders on this PC are not touched.
# Run it once, yourself, from Git Bash:   sh purge_game_data_from_history.sh
set -e
LOCAL="/c/Users/exyle/Xenogears/Game"
REMOTE="https://github.com/exyled/Xenogears-PC"
WORK="$(mktemp -d)"
cd "$WORK"
git clone --bare --no-local "$LOCAL" repo.git
cd repo.git
git config core.autocrlf false
cat > "$WORK/keep.sh" <<'KEEP'
#!/bin/sh
git ls-files -z | grep -z -v -E '^(README\.txt|Xenogears\.exe|\.gitignore|Saves/)' | xargs -0 -r git rm -q --cached --ignore-unmatch --
KEEP
FILTER_BRANCH_SQUELCH_WARNING=1 git filter-branch -f --index-filter "sh $WORK/keep.sh" --prune-empty -- main
echo "--- commits kept:"
git log --format='%h %an | %s' main
echo "--- files in the new history (should be only README.txt, Xenogears.exe, .gitignore, Saves/card1.mcd):"
git log --all --name-only --format= | sort -u
echo
echo "Check the list above. Press Enter to force-push it to $REMOTE, or Ctrl+C to stop."
read _
git push --force "$REMOTE" main
echo "done. GitHub's contributor list and cached views can take a while to refresh."
