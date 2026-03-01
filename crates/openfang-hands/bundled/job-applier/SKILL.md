---
name: job-applier
description: >
  Unified job application pipeline. Orchestrates ApplyPilot (discover, score,
  tailor, apply), GodsScion (LinkedIn Easy Apply), and AIHawk (resume generation)
  across Dockerized containers with programmatic Python tools.
version: 0.2.0
tags:
  - jobs
  - automation
  - pipeline
  - docker
  - career
---

# Job Applier Pipeline

Orchestrates three Dockerized tools into a unified job application workflow.

## Architecture

Three containers orchestrated via `docker compose` from `~/services/job-applier`:

| Container | Role |
|-----------|------|
| openfang-applypilot | Discovery, scoring, tailoring, cover letters, non-LinkedIn apply |
| openfang-godsscion | LinkedIn Easy Apply via stealth bot |
| openfang-aihawk | Interactive resume/cover letter generation |

## Pipeline

1. **Check containers** — verify all 3 are running
2. **Health check** — `applypilot doctor`
3. **Discover** — scrape Indeed, LinkedIn, Glassdoor, ZipRecruiter (4 workers)
4. **Enrich** — fetch full job descriptions (4 workers)
5. **Score** — Gemini rates 1-10 (free tier). 7+ proceed.
6. **Tailor** — rewrite resume per job keywords
7. **Cover letters** — personalized per job
8. **Apply** — LinkedIn via GodsScion stealth bot, everything else via ApplyPilot Playwright
9. **Learn** — store board effectiveness, score thresholds, success rates

## Commands

| Action | Command |
|--------|---------|
| Container status | `docker compose -f ~/services/job-applier/docker-compose.yml ps` |
| Start all | `docker compose -f ~/services/job-applier/docker-compose.yml up -d` |
| Stop all | `docker compose -f ~/services/job-applier/docker-compose.yml down` |
| Health check | `docker compose exec -T applypilot applypilot doctor` |
| Discover | `docker compose exec -T applypilot applypilot run discover --workers 4` |
| Enrich | `docker compose exec -T applypilot applypilot run enrich --workers 4` |
| Score | `docker compose exec -T applypilot applypilot run score` |
| Tailor | `docker compose exec -T applypilot applypilot run tailor` |
| Cover letters | `docker compose exec -T applypilot applypilot run cover` |
| Apply (non-LinkedIn) | `docker compose exec -T applypilot applypilot apply --workers 3` |
| Apply (LinkedIn) | `docker compose exec -T godsscion bash -c "Xvfb :99 & DISPLAY=:99 python runAiBot.py"` |

## Cost

Discover, Enrich, Score, Apply are all **free**. Tailor and Cover use LLM API (configurable provider).
