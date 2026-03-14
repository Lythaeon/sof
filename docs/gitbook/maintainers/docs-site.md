# Docs Site

This page is for maintainers working on the documentation website itself.

External users of SOF do not need this workflow.

## Local Preview

From the repository root:

```bash
cd docs/gitbook
npm install
npm run serve
```

Preview URL:

```text
http://localhost:4000
```

## Static Build

```bash
cd docs/gitbook
npm run build
```

Generated output is written to `docs/gitbook/_book/`.

## GitHub Pages Deployment

The repository includes a Pages workflow at `.github/workflows/docs-pages.yml`.

Deployment model:

- build HonKit output from `docs/gitbook`
- upload `docs/gitbook/_book`
- deploy it through the `github-pages` environment

Repository setting required:

- Pages must be configured to publish from GitHub Actions, not from a branch

## Why This Lives In The Maintainer Track

The docs site build is repository-internal tooling. It is useful when editing documentation, but it
should not be part of the public-facing adoption flow for people who just want to use SOF.
