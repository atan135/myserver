# Backfill Tasks

`db/backfills/<database>/<task-id>/task.json` and its `batch.sql` are the reviewed,
version-controlled definition of one asynchronous data backfill. They are not SQLx
migrations and must not be invoked from a service startup path.

Each task pins its database, logical owner, target migration version, integer cursor,
reviewed batch limits, minimum inter-batch delay and statement timeout. The one
`WITH` statement in `batch.sql` receives `$1` as the prior cursor and `$2` as the
batch size, then returns exactly one row with `next_cursor` and `processed_rows`.
It may update rows in the reviewed task scope but may not manage transactions or
change schema. The directory name, CLI `--task` value and `task.json` `id` must be
identical, so a state/audit record cannot be redirected to another task definition.

The CLI stores runtime state separately in `_myserver_backfill_state` and records
batch, pause, resume and failure events in `_myserver_backfill_audit`. State binds a
SHA-256 task revision, owner and target version. Editing a started task therefore
fails closed; create a new reviewed task id instead. `_sqlx_migrations` remains DDL
history only.

Operational commands require an explicit actor:

```text
node tools/db.js backfill-status --database auth --task example-task
node tools/db.js backfill-run --database auth --task example-task --actor deploy --max-batches 1
node tools/db.js backfill-pause --database auth --task example-task --actor deploy
node tools/db.js backfill-resume --database auth --task example-task --actor deploy
```

Each committed batch releases its advisory lock before the configured delay. Pause
takes effect before the next batch, and resume continues from the durable cursor.
Failed batches are rolled back, marked `failed` with a sanitized audit detail, and
require an explicit resume after the operator repairs the cause. A later
`backfill-run` against a still-failed task returns a nonzero execution result with
the durable state and `failed` reason; it does not silently succeed.
