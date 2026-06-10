CREATE TABLE IF NOT EXISTS links (
  code       text        PRIMARY KEY,
  url        text        NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  hits       bigint      NOT NULL DEFAULT 0
);
