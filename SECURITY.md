# TokenCraft security

TokenCraft is part of ArtCraft’s cyber protection work for open-source infrastructure. It protects organization credentials, project and client boundaries, and the data that ArtCraft apps send through providers or retain for later use. The gateway, SDKs, regression tests, and deployment guidance are available to self-hosters.

## Report a vulnerability

Use the repository’s private vulnerability reporting option where available, or contact an ArtCraft maintainer through an established private channel. Share the affected version or commit, the interface and permissions involved, the observed impact, and supporting evidence from a disposable environment. Redact secrets, private customer data, and production identifiers. Coordinate disclosure privately while maintainers review and fix the issue.

## Review and release

- Check changes against the affected boundary. Examples include project and client authorization, provider credential handling, webhook authenticity, and retained-data access and removal. [Backend design](frontend/public/design/index.html)
- Add regression coverage at the boundary that failed. Verify the fix with the same build and scoped credentials that the release uses, in a disposable environment. [Gateway and SDK checks](https://github.com/storytold/tokencraft/blob/main/.github/workflows/ci.yml)
- Record the tested revision, outstanding risks, and evidence from the applicable release checks. Use the deployment guidance to rehearse schema changes and recovery before rollout. [Deployment and recovery](docs/deployment.md)

## Daybreak

ArtCraft uses Daybreak Defense Factory for defensive review and triage on connected repositories. Track TokenCraft review against the ArtCraft repository and the exact revision reviewed. Record any outstanding review status in the release evidence.

For a confirmed finding, record the maintainer’s disposition, the fix commit, and the checks that verify it. Keep detailed findings in the private review channel until coordinated disclosure. Self-hosters can run the repository’s tests and gateway directly. [OpenAI Codex Security documentation](https://help.openai.com/en/articles/20001107-codex-security)
