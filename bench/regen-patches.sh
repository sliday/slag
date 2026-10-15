#!/usr/bin/env bash
# Regenerates reference.patch for every task. Not part of the bench
# runtime; a maintainer tool. Usage: bench/regen-patches.sh
set -euo pipefail

bench_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

make_patch() {
  local task=$1 fix_py=$2
  local td="$bench_dir/issues/$task"
  local tmp
  tmp=$(mktemp -d -t benchpatch)
  git init -q "$tmp"
  cp -R "$td/repo/." "$tmp/"
  git -C "$tmp" add -A
  git -C "$tmp" -c user.email=bench@localhost -c user.name=bench commit -qm base
  FIX_PY=$fix_py TMP_REPO=$tmp python3 - <<'PYEOF'
import os
import subprocess
subprocess.run(["python3", "-c", os.environ["FIX_PY"]], cwd=os.environ["TMP_REPO"], check=True)
PYEOF
  git -C "$tmp" diff >"$td/reference.patch"
  rm -rf "$tmp"
  echo "$task: $(wc -l <"$td/reference.patch" | tr -d ' ') lines"
}

make_patch py-passcheck '
import pathlib
p = pathlib.Path("passcheck/core.py")
s = p.read_text()
old = "return len(password) >= MIN_LENGTH or (has_upper and has_digit)"
new = "return len(password) >= MIN_LENGTH and (has_upper and has_digit)"
assert s.count(old) == 1
p.write_text(s.replace(old, new))
'

make_patch py-invoice-dates '
import pathlib
p = pathlib.Path("billdate/dates.py")
s = p.read_text()
old = "    return d.replace(month=d.month + months)"
new = """    total = d.year * 12 + (d.month - 1) + months
    year, month = divmod(total, 12)
    month += 1
    day = min(d.day, calendar.monthrange(year, month)[1])
    return date(year, month, day)"""
assert s.count(old) == 1
p.write_text(s.replace(old, new))
'

make_patch py-pricetag-cache '
import pathlib
p = pathlib.Path("shop/pricing.py")
s = p.read_text()
old = """    def update_price(self, sku, new_price):
        \"\"\"Change the price of `sku` in the catalog.\"\"\"
        self.catalog.set_price(sku, new_price)"""
new = """    def update_price(self, sku, new_price):
        \"\"\"Change the price of `sku` in the catalog.\"\"\"
        self.catalog.set_price(sku, new_price)
        self._cache.pop(sku, None)"""
assert s.count(old) == 1
p.write_text(s.replace(old, new))
'

make_patch js-leaderboard '
import pathlib
p = pathlib.Path("src/leaderboard.js")
s = p.read_text()
old = "const sorted = players.slice().sort().reverse();"
new = "const sorted = players.slice().sort((a, b) => b.points - a.points);"
assert s.count(old) == 1
p.write_text(s.replace(old, new))
'

make_patch js-user-pager '
import pathlib
p = pathlib.Path("src/paginate.js")
s = p.read_text()
old = "const start = page * perPage;"
new = "const start = (page - 1) * perPage;"
assert s.count(old) == 1
p.write_text(s.replace(old, new))
p = pathlib.Path("src/api.js")
s = p.read_text()
old = "const totalPages = Math.floor(result.total / perPage);"
new = "const totalPages = Math.ceil(result.total / perPage);"
assert s.count(old) == 1
p.write_text(s.replace(old, new))
'

make_patch js-notify-preview '
import pathlib
p = pathlib.Path("src/preview.js")
s = p.read_text()
old = """function makePreview(text, maxChars = 20) {
  if (text.length <= maxChars) return text;
  return text.slice(0, maxChars) + "…";
}"""
new = """function makePreview(text, maxChars = 20) {
  const chars = Array.from(text);
  if (chars.length <= maxChars) return text;
  return chars.slice(0, maxChars).join("") + "…";
}"""
assert s.count(old) == 1, repr(s)
p.write_text(s.replace(old, new))
'

make_patch rust-hawkconf '
import pathlib
p = pathlib.Path("src/lexer.rs")
lines = p.read_text().splitlines(keepends=True)
hits = [i for i, l in enumerate(lines) if "if c2 ==" in l]
assert len(hits) == 1
i = hits[0]
assert "break;" in lines[i + 1]
indent = lines[i + 1][: len(lines[i + 1]) - len(lines[i + 1].lstrip())]
lines[i + 1] = indent + "line += 1;\n" + lines[i + 1]
p.write_text("".join(lines))
'

make_patch sh-backup-rotate '
import pathlib
p = pathlib.Path("bin/rotate.sh")
s = p.read_text()
old = "| head -n \"$keep\" |"
new = "| tail -n \"+$((keep + 1))\" |"
assert s.count(old) == 1
p.write_text(s.replace(old, new))
'
