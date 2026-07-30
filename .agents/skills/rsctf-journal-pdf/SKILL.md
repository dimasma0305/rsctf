---
name: rsctf-journal-pdf
description: Create, revise, or review RSCTF documentation PDFs using the repository's established A4 two-column journal format, VitePress print classes, PDF profile pipeline, accessibility rules, and page-by-page visual QA. Use for handbook sources under docs/, generated files in docs/public/downloads/, PDF profile changes, or requests to make a PDF match the existing Attack & Defense or KotH papers.
---

# RSCTF Journal PDF

Produce RSCTF papers through the existing documentation pipeline. Treat the
Attack & Defense and KotH handbooks as the visual contract; do not invent a new
layout for an individual paper unless the user explicitly requests one.

## Establish the house style

Before editing, read the relevant parts of:

- `docs/players/attack-defense.md`
- `docs/players/koth-scoring-handbook.md`
- `docs/.vitepress/theme/print.css`
- `docs/.vitepress/theme/koth-print.css`
- `docs/scripts/build-attack-defense-pdf.mjs`
- `docs/package.json`

Use `pageClass: ad-handbook` for an Attack & Defense paper and
`pageClass: koth-handbook` for a KotH or scoring paper. Reuse those classes
instead of adding a paper-specific page mode.

Match these structural conventions:

- A4 portrait pages with the pipeline's standard margins, running header, and
  footer.
- A full-width journal title block followed by the download link, abstract,
  keywords, and status statement.
- The established two-column serif body, including the existing heading,
  caption, table, code, and link treatment.
- Full-width figures and tables with numbered captions immediately below a
  figure or immediately above a table.
- Explicit document status when a paper describes a proposal rather than live
  behavior.

Do not add a single-column override merely to make content fit. Improve the
content, table, figure, or isolated wide element instead.

## Author compatible content

Keep source content readable as Markdown and in the VitePress page:

- Use relative public links that the PDF builder can rewrite.
- Use semantic SVG diagrams with a `<title>` and `<desc>`, legible text, and
  print-safe contrast.
- Keep table headings short. Split an oversized table by topic before reducing
  its text below the established house size.
- Wrap only a genuinely wide formula, table, figure, or code sample in
  `<div class="journal-wide">...</div>` so it spans both columns.
- Use the shared `journal-*` classes; add a new shared rule only when an
  existing class cannot express the requirement.
- Preserve technical truth. Do not present proposed scoring or architecture as
  implemented runtime behavior.

Use native display math supported by the pipeline. Do not use TeX `\tag{...}`;
Chromium's native MathML rendering can turn tagged equations into vertical
character stacks. Refer to equations by descriptive names in prose. Prefer a
short expression that fits one column, and use `journal-wide` for a genuinely
wide expression.

## Register a PDF profile

For a new paper, add the smallest profile entry to `PDF_PROFILES` in
`docs/scripts/build-attack-defense-pdf.mjs`. Supply:

- the VitePress route;
- the output filename under `docs/public/downloads/`;
- the exact title, subject, author, and keywords;
- the running-header title.

Add a focused package script when maintainers need to rebuild the paper alone.
Reuse the existing builder and Chromium setup. Do not duplicate the PDF
pipeline or create a paper-specific build script.

Rebuild only the affected profile during normal editing. Rebuilding unrelated
PDFs changes their generated timestamps and creates needless binary churn.

## Build and inspect

Use the repository-pinned package manager:

```sh
corepack pnpm@11.8.0 --dir docs run <pdf-script>
corepack pnpm@11.8.0 --dir docs run check
```

Then perform all of these checks:

1. Run the PDF builder's built-in assertions without bypassing accessibility,
   image, link, or table checks.
2. Inspect `pdfinfo` for A4 size, the expected metadata, and `Tagged: yes`.
3. Extract text with `pdftotext` and check for missing headings, captions,
   formulas, or unexpected build-page text.
4. Render every page to PNG with `pdftoppm -png`.
5. Visually inspect every rendered page, not only the cover.
6. Reject clipping, overlap, vertical formula stacks, tiny diagram labels,
   awkward blank pages, orphan headings, broken column flow, and tables whose
   meaning is lost across a page break.
7. Compare the generated source artifact with any copied build artifact using
   `cmp` or SHA-256.

Fix problems in the Markdown or shared print styles, rebuild, and repeat the
entire page inspection. Do not declare the PDF ready based only on a successful
build.

## Finish safely

Run the repository's documentation checks and inspect `git diff` plus
`git status`. Keep temporary screenshots and rendered QA pages outside tracked
paths; confirm generated inspection files are ignored.

Commit the intended Markdown, styles, pipeline/profile changes, diagrams, and
final PDF when the user requests a commit. Push and verify the relevant
documentation and CI workflows. For prose- or documentation-only changes,
explicitly report that the production server rebuild was skipped because no
released runtime artifact changed.
