# Pooler

A generic resource pool for [Koja](https://github.com/koja-lang/koja).
Pools any value (database connections, sockets, sessions) behind a
single process that lends resources to callers one at a time.

## Features

- Fixed-size pool built eagerly from a factory closure
- FIFO waitlist: checkout callers block (with a timeout) until a resource frees up
- Checked-in values replace the lent copy, so in-place updates survive the round trip
- Optional `valid` check at checkin drops broken resources before the next borrower sees them
- Empty slots refill with the factory, on a backoff timer when the factory fails

## Installation

Pooler lives in the Koja repository under `examples/pooler` and has no
release of its own. Depend on it by path from a sibling project, the
way the [shortener](../shortener) does:

```toml
[dependencies]
pooler = { path = "../pooler" }
```

## Usage

```koja
alias Pooler.Config
alias Pooler.Pool

config = Config{
  create: fn () -> Result<Connection, String> connect() end,
  size: 5,
  valid: Option.Some(fn (conn: Connection) -> Bool conn.idle?() end),
}

pool = Pool.start(config)

# Borrow a resource, waiting up to 5 seconds.
conn =
  match pool.checkout(5000)
    Result.Ok(conn) -> conn
    Result.Err(e) -> return Result.Err("pool exhausted")
  end

# ... use conn ...

# Return it (the checked-in value is what the next caller receives).
pool.checkin(conn)

# Or report it broken so the pool builds a replacement.
_ = pool.discard(5000)

pool.stop()
```

Checkout failures are a `Pooler.Error`:

| Variant          | Meaning                                          |
| ---------------- | ------------------------------------------------ |
| `Timeout`        | No resource freed up within the checkout timeout |
| `PoolDown`       | The pool process is not running                  |
| `Failed(String)` | The pool answered with an unexpected reply       |

## Not yet supported

- Lease reclamation: a crashed borrower's resource is lost until discarded
- Dynamic resizing and overflow resources
- Idle health checks: `valid` runs at checkin only, never on shelved resources

## Development

```sh
koja test
```

## License

Copyright (c) 2026 Henry Popp

This project is MIT licensed.
