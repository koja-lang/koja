# Pooler

A generic resource pool for [Koja](https://github.com/koja-lang/koja).
Pools any value (database connections, sockets, sessions) behind a
single process that lends resources to callers one at a time.

## Features

- Fixed-size pool built eagerly from a factory closure
- FIFO waitlist: checkout callers block (with a timeout) until a resource frees up
- Checked-in values replace the lent copy, so in-place updates survive the round trip
- Broken resources are discarded and rebuilt with the factory

## Installation

Add the package to your `koja.toml`:

```toml
[dependencies]
Pooler = { github = "hpopp/pooler-koja", tag = "v0.1.0" }
```

## Usage

```koja
alias Pooler.Config
alias Pooler.Pool

config = Config{
  create: fn () -> Result<Connection, String> connect() end,
  size: 5,
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

| Variant          | Meaning                                             |
| ---------------- | --------------------------------------------------- |
| `Timeout`        | No resource freed up within the checkout timeout    |
| `PoolDown`       | The pool process is not running                     |
| `Failed(String)` | The factory failed while rebuilding after a discard |

## Not yet supported

- Lease reclamation: a crashed borrower's resource is lost until discarded
- Dynamic resizing and overflow resources
- Idle health checks

## Development

```sh
koja test
```

## License

Copyright (c) 2026 Henry Popp

This project is MIT licensed.
