-- 0016: Create `minion_attachments` table — per-job binary artifact storage.
--
-- Engine-layer persistence for the minion queue attachment CRUD (add/list/get/
-- delete). A job may carry arbitrary named blobs (manifests, agent outputs,
-- uploaded files); each row is one attachment keyed by (job_id, filename).
--
-- TS reference:
--   - schema:     src/schema.sql (minion_attachments table)
--   - queue ops:  src/core/minions/queue.ts addAttachment/listAttachments/
--                 getAttachment/deleteAttachment
--   - validation: src/core/minions/attachments.ts validateAttachment
--
-- Storage model:
--   Two mutually-exclusive payload channels, enforced by chk_attachment_storage:
--     - `content`     inline BYTEA (the only path the current port populates).
--     - `storage_uri` pointer to external object storage (reserved; always NULL
--                     for now — the inline path is the faithful TS behavior).
--   `size_bytes` and `sha256` are computed at validation time over the decoded
--   bytes. UNIQUE(job_id, filename) is the authoritative dedupe backstop: the
--   app layer does a friendly early-out on existing filenames, but a race still
--   collides on INSERT against this constraint.
--
-- Scope: only minion_attachments. Physically independent from minion_jobs /
--   minion_inbox except for the job_id FK.
--
-- Design decisions (mirror the minion_jobs/minion_inbox port, migrations/0014-0015):
--   - id SERIAL (INTEGER PK AUTOINCREMENT on SQLite).
--   - content BYTEA (BLOB on SQLite).
--   - created_at TIMESTAMPTZ record column (TEXT ISO-8601 on SQLite). Never
--     compared against now() with interval arithmetic, so TEXT is fine there.
--   - job_id FK ON DELETE CASCADE: an attachment has no meaning once its job
--     row is gone (same rationale as minion_inbox).

CREATE TABLE IF NOT EXISTS minion_attachments (
  id            SERIAL PRIMARY KEY,
  job_id        INTEGER NOT NULL REFERENCES minion_jobs(id) ON DELETE CASCADE,
  filename      TEXT NOT NULL,
  content_type  TEXT NOT NULL,
  content       BYTEA,
  storage_uri   TEXT,
  size_bytes    INTEGER NOT NULL,
  sha256        TEXT NOT NULL,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  CONSTRAINT uniq_minion_attachments_job_filename UNIQUE (job_id, filename),
  CONSTRAINT chk_attachment_storage CHECK (content IS NOT NULL OR storage_uri IS NOT NULL),
  CONSTRAINT chk_attachment_size CHECK (size_bytes >= 0)
);

-- Per-job listing/lookup — every attachment query filters by job_id first.
CREATE INDEX IF NOT EXISTS idx_minion_attachments_job ON minion_attachments (job_id);

-- TOAST tuning: attachments are opaque blobs, never substring-searched, so keep
-- them fully out-of-line (uncompressed external storage) to avoid bloating the
-- main heap tuple. No-op on SQLite (omitted there).
ALTER TABLE minion_attachments ALTER COLUMN content SET STORAGE EXTERNAL;
