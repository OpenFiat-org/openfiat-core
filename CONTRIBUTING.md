# Contributing to openfiat-core

Thank you for your interest in contributing to OpenFiat! This document
describes how to propose changes, the standards we hold contributions to,
and how the review process works.

OpenFiat is developed in the open under progressive decentralization.
Organization-wide policies (Code of Conduct, security policy, issue/PR
templates) live in [OpenFiat-org/.github](https://github.com/OpenFiat-org/.github).

## Before you start

- For substantial protocol changes, open an RFC issue using the **RFC**
  template so the design can be discussed before implementation begins.
- For bugs and small enhancements, search existing issues first to avoid
  duplicates.
- By contributing, you agree that your contributions will be licensed under
  the Apache License 2.0 (see [LICENSE](LICENSE)).

## Development workflow

1. Fork the repository and create a feature branch off `main`:
   `git checkout -b feat/short-description`
2. Follow [Conventional Commits](https://www.conventionalcommits.org/) for
   commit messages (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`).
3. Make your changes with accompanying tests and documentation.
4. Run the full local check suite before opening a PR (see repository
   README's **Development** and **Testing** sections for exact commands).
5. Open a pull request against `main` using the PR template. Link any
   related issues.
6. A maintainer will review your PR. Please respond to review feedback
   within a reasonable timeframe; stale PRs may be closed.

## Code style

- Formatting and linting are enforced in CI and via pre-commit hooks
  (see the repository root `.pre-commit-config.yaml`). Run the formatter
  before committing rather than relying solely on CI to catch it.
- Keep pull requests focused. Prefer several small PRs over one large PR
  spanning unrelated concerns.

## Getting help

Open a [Discussion](../../discussions) for questions, or use the **Question**
issue template. Real-time chat and community links are listed in
[awesome-openfiat](https://github.com/OpenFiat-org/awesome-openfiat).
