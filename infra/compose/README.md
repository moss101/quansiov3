# Quansio V8.1 local development stack (GOV-007)

Deterministic, offline-capable dependencies for local development and CI. All
values here are dev-only; never point this stack at real data or credentials.

## Services

| Service | Image | Host endpoint | Role |
|---|---|---|---|
| postgres | `pgvector/pgvector:pg17` (16+) | `127.0.0.1:55440` | authoritative DB; `vector` extension + `derived` schema via `initdb/01-pgvector.sql` |
| nats | `nats:2.11-alpine` | `127.0.0.1:54230` (client), `54231` (monitor) | JetStream event transport |
| minio | `quay.io/minio/minio:latest` | `127.0.0.1:59010` (S3), `59011` (console) | object storage; bucket `quansio-dev` |
| stub-provider | `python:3.12-slim` + `stub-provider/server.py` | `127.0.0.1:59020` | deterministic OpenAI-compatible endpoint for offline model calls |
| vector-adapter | `qdrant/qdrant:v1.12.4` | `127.0.0.1:59030` | optional, profile `vector`, derived index only |

## Commands

```bash
scripts/dev/up            # start the stack (add --vector for the optional adapter) and wait for health
scripts/dev/healthcheck   # exit 0 only when postgres+pgvector, NATS JetStream and MinIO respond
scripts/dev/seed          # load deterministic dev identities (idempotent)
scripts/dev/down          # stop the stack, keep volumes (--volumes also drops data)
scripts/dev/reset         # destroy volumes and return to a deterministic empty baseline
```

`up` writes a generated, gitignored `.env` (dev defaults; never commit it).

## Environment variables

Non-secret settings and defaults live in `config/dev.yaml`; every variable can
be overridden by exporting it before `scripts/dev/up`.

`QUANSIO_DEV_COMPOSE_PROJECT`, `QUANSIO_DEV_POSTGRES_{IMAGE,HOST,PORT,USER,PASSWORD,DB}`,
`QUANSIO_DEV_NATS_{IMAGE,HOST,PORT,MONITOR_PORT}`,
`QUANSIO_DEV_MINIO_{IMAGE,MC_IMAGE,HOST,PORT,CONSOLE_PORT,BUCKET,ROOT_USER,ROOT_PASSWORD}`,
`QUANSIO_DEV_STUB_PROVIDER_{IMAGE,HOST,PORT}`, `QUANSIO_DEV_VECTOR_ADAPTER_{IMAGE,PORT}`,
`QUANSIO_DEV_HEALTH_TIMEOUT`, `QUANSIO_DEV_ENV_FILE`.

Point the model gateway's openai-compatible provider at the stub with
`QUANSIO_OPENAI_COMPATIBLE_BASE_URL=http://127.0.0.1:59020/v1`.

## Seeded dev identities

`dev.seed_identity` (dev-only fixture schema; never canonical authority tables):

| Kind | ID | Slug |
|---|---|---|
| personal tenant | `tn_01J8Z3K6F1DEV0000000000TEN` | `dev-personal` |
| workspace | `ws_01J8Z3K6F1DEV0000000000WKS` | `dev-default` |
| teammate template | `agt_01J8Z3K6F1DEV0000000000AGT` | `dev-teammate` |

## Secrets

`QUANSIO_DEV_POSTGRES_PASSWORD` and `QUANSIO_DEV_MINIO_ROOT_PASSWORD` default to
`quansio-dev-only` for local development only; `scripts/dev/up` writes them into
the gitignored `.env`. Real deployments supply secrets via the secret broker.
