# TokenCraft security

TokenCraft routes requests across AI providers. Its security controls protect provider credentials, enforce project and client isolation, and scope access to retained request data and artifacts.

## Report a vulnerability

Use the repository’s private vulnerability reporting option where available, or contact an ArtCraft maintainer through an established private channel. Share the affected version or commit, the interface and permissions involved, the observed impact, and supporting evidence from a disposable environment. Redact secrets, private customer data, and production identifiers. Coordinate disclosure privately while maintainers review and fix the issue.

## Review and release

- Check changes against the affected boundary. Examples include project and client authorization, provider credential handling, webhook authenticity, and retained-data access and removal. [Backend design](frontend/public/design/index.html)
- Add regression coverage at the boundary that failed. Verify the fix with the same build and scoped credentials that the release uses, in a disposable environment. [Gateway and SDK checks](https://github.com/storytold/tokencraft/blob/main/.github/workflows/ci.yml)
- Record the tested revision, outstanding risks, and evidence from the applicable release checks. Use the deployment guidance to rehearse schema changes and recovery before rollout. [Deployment and recovery](docs/deployment.md)
