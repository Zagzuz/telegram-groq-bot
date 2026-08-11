# Telegram Groq Bot

A small Rust service that answers questions in Telegram groups through Groq. Each Telegram chat has an isolated, bounded conversation window. The service defaults to `openai/gpt-oss-120b` and can switch to `llama-3.1-8b-instant` as the primary model approaches its configured free-plan limits.

## Behavior

The bot responds to `/ask <question>`, messages mentioning the bot, and replies to the bot.

| Command | Purpose |
| --- | --- |
| `/ask <question>` | Ask using this chat's recent context. |
| `/automodel` | Show automatic model-switching status. |
| `/automodel on` | Enable automatic model switching for this chat. |
| `/automodel off` | Stay on the primary model; rate-limited work waits. |
| `/automodel toggle` | Toggle the current setting. |
| `/model` | Show the configured models and switching state. |
| `/reset` | Delete this chat's persisted conversation context. |
| `/privacy` | Show the retention policy. |

In groups and supergroups, `/automodel` changes and `/reset` require a chat administrator. Private-chat users can change their own settings.

## Architecture and routing

Telegram webhooks are validated and reduced to a minimal durable job before the endpoint returns `200`. A Tokio worker processes queued jobs, with only one in-flight job allowed per chat. Telegram `update_id` values provide deduplication.

The Groq router uses response rate-limit headers and one daily aggregate row per model. It chooses the fallback when:

- Estimated primary usage reaches `PRIMARY_DAILY_SWITCH_PERCENT` of its daily token budget.
- A request would consume the configured minute-token reserve.
- The primary has 10% or less of its daily request allowance remaining.
- The primary is cooling down after a `429` response.

If both models lack capacity, the job is deferred. The service does not rotate API keys. When a chat disables automatic switching, a primary-model rate limit defers its work instead of changing models.

When a chat changes models, the first answer from the new model starts with a visible `previous → current` notification. The first answer ever generated for a chat does not announce a switch.

## Minimal persistence

PostgreSQL stores:

- At most `CONTEXT_MAX_MESSAGES` messages per chat, additionally capped by `CONTEXT_MAX_CHARS` and `CONTEXT_RETENTION_DAYS`.
- The switching setting and last-used model ID for each active chat.
- Pending jobs; completed job data is deleted immediately after delivery.
- Telegram update IDs without message bodies for 48 hours.
- One token-usage aggregate per model per UTC day, retained for two days.

Usernames, raw Telegram updates, and group metadata are not retained. Defaults are 12 messages, 16 KiB, and seven days per chat.

## Prerequisites

- A Telegram bot token from BotFather.
- A Groq API key.
- PostgreSQL 14 or newer. Neon Postgres works well for free deployments.
- Rust 1.85 or newer for native development, or Docker.

For group use, add the bot to the group. Telegram privacy mode may remain enabled because commands, mentions, and replies are sufficient.

## Local development

Copy `.env.example` to `.env`, set the secrets, and start PostgreSQL and the bot:

```sh
docker compose up --build
```

Telegram requires a public HTTPS webhook. Expose port 8080 with a trusted HTTPS tunnel, set `PUBLIC_BASE_URL`, then register it:

```sh
cargo run -- set-webhook
```

Other commands:

```sh
cargo run -- migrate
cargo run -- serve
cargo run -- delete-webhook
```

Endpoints:

- `GET /healthz` — process liveness.
- `GET /readyz` — PostgreSQL readiness.
- `POST /telegram/webhook` — Telegram webhook receiver.

## Configuration

Required variables:

| Variable | Description |
| --- | --- |
| `DATABASE_URL` | PostgreSQL URL; use `sslmode=require` with hosted databases. |
| `TELEGRAM_BOT_TOKEN` | Telegram Bot API token. |
| `TELEGRAM_WEBHOOK_SECRET` | Random 1–256 character webhook secret. |
| `TELEGRAM_BOT_USERNAME` | Username without the leading `@`. |
| `GROQ_API_KEY` | Groq API key. |

Important optional values:

| Variable | Default |
| --- | --- |
| `GROQ_PRIMARY_MODEL` | `openai/gpt-oss-120b` |
| `GROQ_FALLBACK_MODEL` | `llama-3.1-8b-instant` |
| `PUBLIC_BASE_URL` | unset |
| `AUTO_REGISTER_WEBHOOK` | `false` |
| `PORT` | `8080` |
| `CONTEXT_MAX_MESSAGES` | `12` |
| `CONTEXT_MAX_CHARS` | `16384` |
| `CONTEXT_RETENTION_DAYS` | `7` |
| `PRIMARY_ANSWER_MAX_TOKENS` | `800` |
| `FALLBACK_ANSWER_MAX_TOKENS` | `500` |
| `PRIMARY_DAILY_TOKEN_BUDGET` | `200000` |
| `FALLBACK_DAILY_TOKEN_BUDGET` | `500000` |
| `PRIMARY_DAILY_REQUEST_BUDGET` | `1000` |
| `FALLBACK_DAILY_REQUEST_BUDGET` | `14400` |
| `PRIMARY_DAILY_SWITCH_PERCENT` | `80` |
| `RATE_RESERVE_PERCENT` | `20` |

Set `AUTO_REGISTER_WEBHOOK=true` only after `PUBLIC_BASE_URL` points to the deployed HTTPS service. Registration is idempotent and occurs during startup.

## Docker

```sh
docker build -t telegram-groq-bot .
docker run --rm --env-file .env -p 8080:8080 telegram-groq-bot
```

The image runs as an unprivileged user and embeds migrations in the binary. `serve` applies migrations before accepting requests.

## Free deployment: Northflank and Neon

1. Create a Neon project and copy its pooled PostgreSQL connection URL.
2. Create a Northflank combined service from this repository and build it with the root `Dockerfile`.
3. Expose public HTTP port `8080` and configure `/healthz` as the readiness path.
4. Add the required environment variables, using the Neon URL for `DATABASE_URL`.
5. Set `PUBLIC_BASE_URL` to the Northflank `code.run` HTTPS URL and `AUTO_REGISTER_WEBHOOK=true`.
6. Deploy, check `/readyz`, and run `/model` in Telegram.

Use a Neon region close to Northflank. The service uses ephemeral local storage, so PostgreSQL must remain external. The service tolerates restarts because pending work and chat context are durable.

## Operational notes

- Groq and Telegram secrets are read only from environment variables and are not logged.
- Model-supplied HTML is escaped; only formatting generated by the server-side renderer is sent to Telegram.
- Replies are limited to two Telegram messages and split on UTF-8-safe boundaries. Oversized model output is truncated with an ellipsis. Per-chunk progress is saved so a transient failure resumes instead of resending the entire answer.
- Groq's GitHub-style Markdown is converted server-side to Telegram Rich HTML before delivery, including headings, lists, links, quotations, code blocks, and native inline or display LaTeX formulas.
- When model capacity requires a retry, the bot sends one immediate queue notice with the next-attempt estimate. A single boolean on the pending job prevents repeated notices and is deleted with the completed job.
- The health-check workflow requests the deployed Northflank `/healthz` endpoint every ten minutes. GitHub automatically disables scheduled workflows in public repositories after 60 days without repository activity.
- Queue, model selection, Groq latency, deferral, Telegram delivery, and completion events are emitted as structured logs without message contents.
- A crash after Telegram accepts a chunk but before its progress update commits can still duplicate that chunk; Telegram does not accept an idempotency key for `sendRichMessage`.
