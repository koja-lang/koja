# Shortener

A URL shortener written in Koja, and a complete end-to-end CRUD
service. It serves HTTP on `Net.TCPServer` + `HTTP.Parser`, routes
requests through [Trail](https://github.com/koja-lang/trail), and
reads and writes `JSON`. Links live in PostgreSQL, reached through
the [Postgres](https://github.com/hpopp/postgres-koja) package, a
pure-Koja driver speaking the v3 wire protocol (no C driver, no FFI),
with connections managed by the
[Pooler](https://github.com/koja-lang/pooler) resource pool.

It doubles as a tour of the things that make Koja great.

- **Process entry.** The program starts from `App`
  (`struct App: Process<List<String>, TCPEvent, String>` in `src/app.koja`),
  named by `entry = "App"` in `koja.toml`.
- **Event-driven accept.** A spawned `Net.TCPServer` watches the
  listener with the runtime's IO reactor and delivers each accepted
  socket as a `TCPEvent.Connected` message. Nothing polls.
- **Process per connection.** `App` hands every socket to a one-shot
  `Worker` process, so slow clients and concurrent requests never
  block each other. Workers speak HTTP keep-alive, framing requests
  off the socket with a buffered reader.
- **Signal-driven lifecycle.** `handle_signal` receives `Lifecycle`
  events, so Ctrl-C (`Interrupt`) and SIGTERM (`Shutdown`) exit with
  the right code.
- **Routing as plain functions.** `Trail.Router` holds the route
  table, and every handler is a `fn (Conn) -> Conn` closure that
  captured the pool when the table was built. The router decodes
  path and query parameters, binds `:code`, and answers 400, 404,
  and 405 itself through one `on_reject` hook, so the service's JSON
  error shape covers them too.
- **Value semantics.** Each handler checks a Postgres connection out
  of the pool as a `Lease`. Every query returns an updated connection,
  which the handler stores back on the lease before checking it in.
  No locks, no mutation at a distance.
- **Process-based pooling.** `Pooler.Pool` holds 10 connections
  behind a single process that lends them out one at a time, queueing
  checkout callers in FIFO order when all are lent. The pool builds in
  the background, so startup never waits on Postgres and requests
  answer 503 until the first connection is ready. A connection that
  comes back closed or mid-transaction fails the pool's `valid` check
  (`conn.idle?()`), so the pool drops it and rebuilds the slot, on a
  backoff timer if Postgres is down.
- **Git dependencies.** The driver, the pool, and the router are
  declared in `koja.toml` by GitHub tag (for example
  `trail = { github = "koja-lang/trail", tag = "v0.1.0" }`) and pinned
  to exact commits by the committed `koja.lock`.

## Layout

```
api-docs/         -- Bruno collection covering every route
db/init.sql       -- schema, applied automatically by the compose stack
src/
  app.koja        -- entry process, per-connection workers, keep-alive
  config.koja     -- env-driven runtime configuration
  json_util.koja  -- request-body parsing / response encoding helpers
  links.koja      -- LinkStore, CRUD queries over the Postgres driver
  router.koja     -- route table, handlers, and the JSON error shape
test/             -- unit tests (no database required)
```

## Running

Fetch dependencies. This writes `deps/` from the pins in `koja.lock`.

```sh
koja deps get
```

Start Postgres. It listens on host port 5433 and applies the schema
on first boot.

```sh
docker compose up -d
```

Build and run the service.

```sh
koja run
```

Then exercise it.

```sh
# create a link
curl -s -X POST localhost:8080/links -d '{"url": "https://example.com"}'

# list links (paginated, newest first) / fetch metadata
curl -s 'localhost:8080/links?limit=20&offset=40'
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

| Method   | Path           | Behavior                                                                 |
| -------- | -------------- | ------------------------------------------------------------------------ |
| `GET`    | `/`            | Service info                                                             |
| `GET`    | `/links`       | List links, newest first (`limit` up to 1000, default 100, and `offset`) |
| `POST`   | `/links`       | Create a link from `{"url": "..."}`                                      |
| `GET`    | `/links/:code` | Link metadata (url, hits, created_at)                                    |
| `PUT`    | `/links/:code` | Repoint a link at a new URL                                              |
| `DELETE` | `/links/:code` | Delete a link                                                            |
| `GET`    | `/:code`       | 302-redirect to the target, count the hit                                |

## Configuration

All settings come from the environment, with defaults matching the
compose stack.

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

The unit tests cover the JSON helpers and the routes that answer
before any database work (service info, 400, 404, 405, and the 503
a stopped pool produces). They run without a database. The pool
logs one error per test when its factory fails, which is expected.
