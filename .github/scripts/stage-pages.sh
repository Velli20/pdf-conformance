#!/usr/bin/env bash
# Stages the downloaded reports as the GitHub Pages site: each viewer under /<report>/ and a
# landing page. Only the viewer and what it links to are published; results.json and issue
# bodies stay in the run artifacts.
#   stage-pages.sh <downloaded artifacts> <site>
# Needs SAFE_PDF_REPOSITORY, SAFE_SHA, RUN_URL and the GitHub Actions environment.
set -euo pipefail

reports=${1:?usage: stage-pages.sh <reports> <site>}
site=${2:?usage: stage-pages.sh <reports> <site>}
mkdir -p "$site"
items=""
# The PDFium corpus is rendered by both renderers; the pdf.js corpus is only read.
for artifact in conformance-pdfium reads-pdfjs conformance-pdfium-web; do
  report=${artifact#*-}
  src="$reports/$artifact"
  if [ ! -f "$src/index.html" ]; then
    items="$items<li>$report: no report in this run</li>"
    continue
  fi
  rm -rf "$src/results.json" "$src/issues"
  mv "$src" "$site/$report"
  totals=$(grep -m1 -E '^[0-9]+ documents:' "$site/$report/TRIAGE.md" || true)
  items="$items<li><a href=\"$report/\">$report</a> · <a href=\"$report/TRIAGE.md\">TRIAGE.md</a><br><span>$totals</span></li>"
done

size=$(du -sm "$site" | cut -f1)
if [ "$size" -gt 900 ]; then
  echo "::warning ::The Pages site is ${size} MB; GitHub Pages sites are limited to 1 GB."
fi

owner=$(echo "${GITHUB_REPOSITORY_OWNER:?}" | tr '[:upper:]' '[:lower:]')
commit_url="https://github.com/${SAFE_PDF_REPOSITORY:?}/commit/${SAFE_SHA:?}"
cat > "$site/index.html" <<HTML
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Safe-PDF conformance</title>
<style>
  :root { color-scheme: light dark; font: 15px system-ui, sans-serif; }
  body { max-width: 720px; margin: 32px auto; padding: 0 16px; }
  li { margin: 12px 0; }
  span, p { color: GrayText; }
</style>
</head>
<body>
<h1>Safe-PDF conformance</h1>
<p>Safe-PDF compared with PDFium, with pdf.js and WebKit shown next to failing pages. <code>pdfium</code> and <code>pdfjs</code> render page content without annotations or form fields; <code>pdfium-web</code> renders the browser stack with its annotation layer, compared with PDFium's render of annotations and form fields.</p>
<ul>$items</ul>
<p>Safe-PDF <a href="$commit_url">${SAFE_SHA::7}</a> · <a href="${RUN_URL:?}">workflow run</a> · $(date -u '+%Y-%m-%d %H:%M UTC')</p>
<p><a href="https://${owner}.github.io/${SAFE_PDF_REPOSITORY#*/}/">Safe-PDF web viewer</a></p>
</body>
</html>
HTML
