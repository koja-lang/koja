# Shortener

A URL shortener written in Koja — a complete end-to-end CRUD service:
HTTP serving on `Net.TCPListener` + `HTTP.Parser`, JSON in and out via
`JSON`, and PostgreSQL through the [Postgres](https://github.com/hpopp/postgres-koja)
package, a pure-Koja driver speaking the v3 wire protocol (no C
driver, no FFI), with connections managed by the
[Pooler](../pooler) resource pool.

It doubles as a tour of the things that make Koja great:

- **Process entry** — the program starts from `App`
  (`impl Process<List<String>, TCPEvent, String>` in `src/app.koja`),
  named by `entry = "App"` in `koja.toml`.
- **Event-driven accept** — a spawned `Net.TCPServer` watches the
  listener with the runtime's IO reactor and delivers each accepted
  socket as a `TCPEvent.Connected` message; nothing polls.
- **Process per connection** — `App` hands every socket to a one-shot
  `Worker` process, so slow clients and concurrent requests never
  block each other. Workers speak HTTP keep-alive, framing requests
  off the socket with a buffered reader.
- **Signal-driven lifecycle** — `handle_signal` receives `Lifecycle`
  events, so Ctrl-C (`Interrupt`) and SIGTERM (`Shutdown`) exit with
  the right code.
- **Value semantics** — each request checks a Postgres connection out
  of the pool; every query returns an updated connection that the
  router threads through, and the updated value is checked back in.
  No locks, no mutation at a distance.
- **Process-based pooling** — `Pooler.Pool` holds 10 eagerly built
  connections behind a single process that lends them out one at a
  time, queueing checkout callers in FIFO order when all are lent.
- **Git and path dependencies** — the driver is declared in
  `koja.toml` (`Postgres = { github = "hpopp/postgres-koja", tag = "v0.1.0" }`)
  and pinned to an exact commit by the committed `koja.lock`, while
  the pool comes from a local path (`Pooler = { path = "../pooler" }`).

## Layout

```
api-docs/         -- Bruno collection covering every route
db/init.sql       -- schema, applied automatically by the compose stack
src/
  app.koja        -- entry process, per-connection workers, keep-alive
  config.koja     -- env-driven runtime configuration
  json_util.koja  -- request-body parsing / response encoding helpers
  links.koja      -- LinkStore: CRUD queries over the Postgres driver
  router.koja     -- request -> response dispatch
test/             -- unit tests (no database required)
```

## Running

Fetch dependencies (writes `deps/` from the pins in `koja.lock`):

```sh
koja deps get
```

Start Postgres (listens on host port 5433, schema applied on first boot):

```sh
docker compose up -d
```

Build and run the service:

```sh
koja run
```

Then exercise it:

```sh
# create a link
curl -s -X POST localhost:8080/links -d '{"url": "https://example.com"}'

# list links / fetch metadata
curl -s localhost:8080/links
curl -s localhost:8080/links/<code>

# repoint and delete
curl -s -X PUT localhost:8080/links/<code> -d '{"url": "https://koja.dev"}'
curl -s -X DELETE localhost:8080/links/<code>

# follow a short link (302, counts the hit)
curl -sv localhost:8080/<code>
```

The `api-docs/` directory contains a [Bruno](https://www.usebruno.com)
collection with a request per route.

## Routes

| Method   | Path           | Behavior                                  |
| -------- | -------------- | ----------------------------------------- |
| `GET`    | `/`            | Service info                              |
| `GET`    | `/links`       | List every link                           |
| `POST`   | `/links`       | Create a link from `{"url": "..."}`       |
| `GET`    | `/links/:code` | Link metadata (url, hits, created_at)     |
| `PUT`    | `/links/:code` | Repoint a link at a new URL               |
| `DELETE` | `/links/:code` | Delete a link                             |
| `GET`    | `/:code`       | 302-redirect to the target, count the hit |

## Configuration

All settings come from the environment, with defaults matching the
compose stack:

| Variable      | Default     | Purpose           |
| ------------- | ----------- | ----------------- |
| `PORT`        | `8080`      | HTTP listen port  |
| `DB_HOST`     | `127.0.0.1` | Postgres host     |
| `DB_PORT`     | `5433`      | Postgres port     |
| `DB_USER`     | `postgres`  | Postgres user     |
| `DB_PASSWORD` | `shortener` | Postgres password |
| `DB_NAME`     | `shortener` | Database name     |

The compose database authenticates with SCRAM-SHA-256 (the postgres
image's default when a password is set), which the driver negotiates
during the connection handshake.

## Tests

```sh
koja test
```

The unit tests cover the JSON helpers and run without a database.
