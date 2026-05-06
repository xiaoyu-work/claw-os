# Vendored Hermes Skills

This directory contains a curated subset of skills vendored from the
[Hermes Agent](https://github.com/NousResearch/hermes-agent) project by
Nous Research, used here as agent skill packages under the
[agentskills.io](https://agentskills.io) protocol.

## License

Hermes Agent is distributed under the **MIT License**:

> Copyright (c) 2025 Nous Research
>
> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all
> copies or substantial portions of the Software.

The full license text is available upstream at
<https://github.com/NousResearch/hermes-agent/blob/main/LICENSE>.

Some individual skills carry their own LICENSE / LICENSE.txt files that are
preserved alongside their `SKILL.md` — those terms also apply.

## What was vendored

Twenty-five skills were selected for general-purpose engineering, research and
agent productivity. The directory layout under `hermes/` mirrors upstream
`skills/<category>/<skill-name>/` exactly so that contributors can diff against
the upstream tree.

| Category | Skill |
|---|---|
| software-development | test-driven-development |
| software-development | systematic-debugging |
| software-development | plan |
| software-development | writing-plans |
| software-development | spike |
| software-development | requesting-code-review |
| software-development | subagent-driven-development |
| github | codebase-inspection |
| github | github-issues |
| github | github-pr-workflow |
| github | github-code-review |
| github | github-repo-management |
| research | arxiv |
| research | blogwatcher |
| mlops/inference | llama-cpp |
| mlops | huggingface-hub |
| productivity | notion |
| productivity | linear |
| productivity | ocr-and-documents |
| productivity | google-workspace |
| creative | architecture-diagram |
| creative | design-md |
| creative | creative-ideation |
| mcp | native-mcp |
| data-science | jupyter-live-kernel |

## What was *not* vendored

ClawOS keeps its own kernel-primitive reference skills under `skills/claw-os/`
— these are separate from the Hermes vendor tree and are not derivative works.

The full Hermes corpus (89 skills + an `optional-skills/` tree) was not
vendored; users who need additional skills can copy them from the upstream
repository under the same MIT terms.
